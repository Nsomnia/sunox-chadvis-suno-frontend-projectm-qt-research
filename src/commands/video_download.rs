//! Default direct song-video downloads and unchanged audio extraction.
use serde::Serialize;
use serde_json::{Value, json};

use crate::api::types::Clip;
use crate::api::{PollingOptions, SunoClient};
use crate::app::AppContext;
use crate::cli::{DownloadArgs, DownloadFormat};
use crate::core::{CliError, ensure_clip_ids, ensure_poll_timeout_secs};
use crate::media::{
    download as files,
    video::{MediaQuality, VideoTools},
};
use crate::output::{self, OutputFormat};
use crate::workflow::{tasks, visual};

#[derive(Serialize)]
struct VideoDownload {
    clip_id: String,
    path: String,
    source: &'static str,
    format: &'static str,
    video_generated: bool,
    audio_reencoded: bool,
    source_media: MediaQuality,
    output_media: MediaQuality,
}

pub(super) async fn download(args: DownloadArgs, ctx: &AppContext) -> Result<(), CliError> {
    if args.video && args.format.is_some() {
        return Err(CliError::Config(
            "--video cannot be combined with --format".into(),
        ));
    }
    if args
        .format
        .is_some_and(|format| format != DownloadFormat::M4a)
    {
        return Err(CliError::Config("--source video copies the original audio into M4A; MP3/WAV/OPUS conversion is not supported; allowance exports require explicit --source official".into()));
    }
    if args.no_convert {
        return Err(CliError::Config("--no-convert applies to official WAV downloads; --source video always preserves the original stream".into()));
    }
    ensure_clip_ids(&args.ids)?;
    let mut seen = std::collections::HashSet::new();
    let ids: Vec<_> = args
        .ids
        .iter()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect();
    let timeout = args.video_timeout.unwrap_or(ctx.config.poll_timeout_secs);
    ensure_poll_timeout_secs(timeout)?;
    let polling = PollingOptions {
        timeout: std::time::Duration::from_secs(timeout),
        interval: std::time::Duration::from_secs(ctx.config.poll_interval_secs),
    };
    polling.validate()?;
    let directory = args.output.as_deref().unwrap_or(&ctx.config.output_dir);
    let ext = if args.video { "mp4" } else { "m4a" };
    // Dependencies are checked before authentication or creating any video.
    let tools = VideoTools::discover(!args.video).await?;
    let client = ctx.client().await?;
    let clips = tasks::require_found_clips(&ids, client.get_clips(&ids).await?)?;
    let mut paths = std::collections::HashSet::new();
    for clip in &clips {
        if clip.status != "complete" || clip.is_trashed == Some(true) {
            return Err(CliError::Config(format!(
                "video source {} must be complete and not trashed",
                clip.id
            )));
        }
        validate_video_metadata(clip)?;
        let path = files::planned_clip_download_path(clip, directory, ext);
        if !paths.insert(path) {
            return Err(CliError::Download(
                "video sources resolve to the same destination file".into(),
            ));
        }
    }
    // Refuse existing/unwritable destinations for the entire batch before any render.
    for clip in &clips {
        files::preflight_clip_download(clip, directory, ext, args.force).await?;
    }
    let mut completed = Vec::new();
    let mut generated = Vec::new();
    for (index, clip) in clips.iter().enumerate() {
        let mut stage = "video_lookup";
        let result = async {
            let (url, created) = resolve_video(&client, clip, polling, ctx).await?;
            if created {
                generated.push(clip.id.clone());
            }
            stage = "video_transfer";
            // Keep unrequested MP4s in an owned temporary directory, cleaned on all exits.
            let temporary = if args.video {
                None
            } else {
                Some(tempfile::tempdir_in(directory)?)
            };
            let staging_directory = temporary
                .as_ref()
                .map(|dir| dir.path())
                .unwrap_or_else(|| std::path::Path::new(directory));
            let staging_directory = staging_directory
                .to_str()
                .ok_or_else(|| CliError::Config("temporary directory must be UTF-8".into()))?;
            let video = files::stage_clip_url_direct(
                clip,
                staging_directory,
                &url,
                "mp4",
                args.video && args.force,
                ctx.quiet,
            )
            .await?;
            stage = "media_inspection";
            let quality = tools.inspect(video.path()).await?;
            if !quality
                .streams
                .iter()
                .any(|stream| stream.codec_type == "video")
            {
                return Err(CliError::Download(
                    "returned MP4 has no video stream".into(),
                ));
            }
            stage = "local_output";
            let (path, output_quality) = if args.video {
                (video.validate_and_commit("mp4").await?, quality.clone())
            } else {
                tools
                    .extract_audio(clip, video.path(), &quality, directory, args.force)
                    .await?
            };
            Ok::<_, CliError>(VideoDownload {
                clip_id: clip.id.clone(),
                path,
                source: "video",
                format: ext,
                video_generated: created,
                audio_reencoded: false,
                source_media: quality,
                output_media: output_quality,
            })
        }
        .await;
        match result {
            Ok(item) => {
                if !ctx.quiet {
                    eprintln!("Downloaded from song video: {}", item.path);
                }
                completed.push(item);
            }
            Err(error) => {
                return Err(progress_error(
                    error,
                    clip,
                    stage,
                    &completed,
                    &generated,
                    &ids[index + 1..],
                ));
            }
        }
    }
    if matches!(ctx.fmt, OutputFormat::Json) {
        output::json::success(completed);
    }
    Ok(())
}

fn validate_video_metadata(clip: &Clip) -> Result<(), CliError> {
    if clip
        .metadata
        .extra
        .get("video_is_stale")
        .is_some_and(|value| !matches!(value, Value::Null | Value::Bool(_)))
    {
        return Err(CliError::Api {
            code: "schema_drift",
            message: "video_is_stale must be boolean or null".into(),
        });
    }
    Ok(())
}

async fn resolve_video(
    client: &SunoClient,
    clip: &Clip,
    polling: PollingOptions,
    ctx: &AppContext,
) -> Result<(String, bool), CliError> {
    if let Some(url) = clip.direct_video_url() {
        return Ok((url.to_owned(), false));
    }
    // Hold the account lock across the fresh lookup, state check and any render.
    // Read-only recovery can inspect/poll an existing job without acquiring a write lock.
    let _guard = if ctx.read_only {
        None
    } else {
        ctx.acquire_mutation_lock_for(&client.auth_state_snapshot())?
    };
    let current = if ctx.read_only {
        clip.clone()
    } else {
        client
            .get_clip(&clip.id)
            .await?
            .ok_or_else(|| CliError::NotFound(format!("clip {}", clip.id)))?
    };
    if current.id != clip.id {
        return Err(CliError::Api {
            code: "schema_drift",
            message: "video source readback returned a different clip".into(),
        });
    }
    validate_video_metadata(&current)?;
    if let Some(url) = current.direct_video_url() {
        return Ok((url.to_owned(), false));
    }
    // Clip metadata can lag behind a running or finished render. Never infer that
    // a missing clip.video_url means no job exists, including after an earlier timeout.
    let existing = client.inspect_video_generation_status(&clip.id).await?;
    if existing.is_complete()
        && existing.video_is_stale != Some(true)
        && existing
            .video_url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
    {
        return video_status_url(current, existing, false);
    }
    if !existing.is_complete() && !existing.is_failure() {
        let status = visual::wait_for_existing_video(client, &clip.id, polling).await?;
        return video_status_url(current, status, false);
    }
    // Only terminal absence/staleness or a confirmed failure permits a fresh render.
    ctx.ensure_mutations_allowed()?;
    let status = visual::generate_video_and_wait(client, &clip.id, polling).await?;
    video_status_url(current, status, true)
}

fn video_status_url(
    mut clip: Clip,
    status: crate::api::types::VideoGenerationStatus,
    created: bool,
) -> Result<(String, bool), CliError> {
    clip.video_url = status.video_url;
    clip.metadata.extra.insert(
        "video_is_stale".into(),
        json!(status.video_is_stale.unwrap_or(false)),
    );
    let Some(url) = clip.direct_video_url() else {
        if created {
            return Err(CliError::PartialMutation {
                message: "video generation completed but its returned URL is not usable".into(),
                details: json!({"clip_id":clip.id,"generated_video_clip_ids":[clip.id],"stage":"generated_video_url",
                    "recovery":{"resumable":false,"inspection_commands":[format!("sunox clip video-status {} --json",clip.id)]}}),
            });
        }
        return Err(CliError::Download(format!(
            "existing video status for clip {} has no usable current video URL; no render or download authorization was submitted",
            clip.id
        )));
    };
    Ok((url.to_owned(), created))
}

fn progress_error(
    mut error: CliError,
    clip: &Clip,
    stage: &str,
    completed: &[VideoDownload],
    generated: &[String],
    remaining: &[String],
) -> CliError {
    if let CliError::AmbiguousMutation { details, .. } = &mut error {
        details["completed_downloads"] = json!(completed);
        details["generated_video_clip_ids"] = json!(generated);
        details["not_attempted_clip_ids"] = json!(remaining);
        return error;
    }
    if completed.is_empty() && generated.is_empty() {
        return error;
    }
    CliError::PartialDownload {
        message: format!("video download stopped at {stage} for clip {}", clip.id),
        details: json!({"succeeded":completed,"generated_video_clip_ids":generated,
            "failed":{"clip_id":clip.id,"stage":stage,"code":error.error_code(),"message":error.to_string(),"details":error.details()},
            "not_attempted_clip_ids":remaining,
            "recovery":{"resumable":false,"inspection_commands":[format!("sunox clip video-status {} --json",clip.id)],
            "guidance":"Inspect video status and retry --source video --read-only; do not repeat an uncertain generation."}}),
    }
}
