use super::support::{ChallengeMode, execute_generation_submission, output_generation};
use crate::api::types::{AudioReference, ControlSliders, GenerateRequest};
use crate::app::AppContext;
use crate::cli::{ChallengeArgs, SimpleArgs, SoundsArgs, SpeechArgs, VocalGender};
use crate::core::CliError;
use crate::workflow::image_upload;

pub async fn speech(args: SpeechArgs, ctx: &AppContext) -> Result<(), CliError> {
    let script = match (args.script, args.script_file) {
        (Some(script), _) => Some(script),
        (_, Some(file)) => Some(tokio::fs::read_to_string(file).await?),
        _ => None,
    };
    let mut req = GenerateRequest::new(
        "chirp-goose",
        if script.is_some() { "custom" } else { "simple" },
    );
    req.metadata.is_speech = Some(true);
    if let Some(script) = script {
        req.prompt = nonempty(&script)?;
        req.title = Some(String::new());
        req.tags = Some(args.tone.unwrap_or_default());
        req.metadata.backing_music = Some(!args.no_music);
        req.metadata.control_sliders = args.variety.map(|variety| ControlSliders {
            aug_creativity: Some(f64::from(variety)),
            weirdness_constraint: None,
            style_weight: None,
            audio_weight: None,
        });
    } else {
        req.task = Some("agentic_thinking".into());
        req.gpt_description_prompt = Some(nonempty(args.describe.as_deref().unwrap_or_default())?);
        req.metadata.lyrics_model = Some("default".into());
    }
    req.metadata.vocal_gender = args.vocal.as_ref().map(|gender| {
        match gender {
            VocalGender::Male => "m",
            VocalGender::Female => "f",
        }
        .into()
    });
    submit(req, args.challenge, ctx).await
}

pub async fn sounds(args: SoundsArgs, ctx: &AppContext) -> Result<(), CliError> {
    let mut req = GenerateRequest::new(
        args.model.as_deref().unwrap_or(&ctx.config.default_model),
        "custom",
    );
    req.task = Some("sound".into());
    req.make_instrumental = true;
    req.tags = Some(nonempty(&args.prompt)?);
    req.title = Some(args.prompt.chars().take(100).collect());
    let mut config = serde_json::json!({"user_loop":args.r#loop});
    if let Some(bpm) = args.bpm {
        config["user_tempo"] = serde_json::json!(bpm);
    }
    if let Some(key) = args.key {
        let allowed = [
            "C", "C#", "Db", "D", "D#", "Eb", "E", "F", "F#", "Gb", "G", "G#", "Ab", "A", "A#",
            "Bb", "B",
        ];
        let root = key
            .strip_suffix(" minor")
            .or_else(|| key.strip_suffix(" major"))
            .unwrap_or(&key);
        if !allowed.contains(&root) {
            return Err(CliError::Config(
                "--key must be a musical key such as C, F# minor or Bb major".into(),
            ));
        }
        config["user_key"] = serde_json::json!(key);
    }
    req.metadata.sound_configs = Some(config);
    submit(req, args.challenge, ctx).await
}

pub async fn simple(args: SimpleArgs, ctx: &AppContext) -> Result<(), CliError> {
    if args.audio_refs.iter().any(|id| id.trim().is_empty()) {
        return Err(CliError::Config(
            "audio reference IDs must not be blank".into(),
        ));
    }
    let mut req = GenerateRequest::new(
        args.model.as_deref().unwrap_or(&ctx.config.default_model),
        "simple",
    );
    req.task = Some("agentic_thinking".into());
    req.gpt_description_prompt = Some(nonempty(&args.prompt)?);
    req.metadata.lyrics_model = Some("default".into());
    req.attached_lyrics = args.lyrics;
    req.attached_styles = args.styles;
    let mut images = Vec::new();
    let mut prepared_bytes = 0usize;
    for file in &args.image_files {
        let image = image_upload::prepare_attachment(file).await?;
        prepared_bytes = prepared_bytes
            .checked_add(image.byte_len())
            .ok_or_else(|| {
                CliError::Config("image attachments exceed the local memory budget".into())
            })?;
        if prepared_bytes > 64 * 1024 * 1024 {
            return Err(CliError::Config(
                "image attachments exceed the CLI's combined 64 MiB memory limit".into(),
            ));
        }
        images.push(image);
    }
    for id in &args.image_ids {
        uuid::Uuid::parse_str(id).map_err(|_| {
            CliError::Config("--image-id must be a completed Suno image upload UUID".into())
        })?;
    }
    req.user_uploaded_image_ids = args.image_ids;
    // Validate reference IDs without truncation or silent deduplication.
    let distinct = args
        .audio_refs
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if distinct.len() != args.audio_refs.len() {
        return Err(CliError::Config("audio references must be distinct".into()));
    }
    let ChallengeArgs {
        token,
        token_provider,
        captcha,
        no_captcha,
    } = args.challenge;
    let result = execute_generation_submission(
        token,
        token_provider,
        ChallengeMode::from_flags(captcha, no_captcha),
        ctx,
        move |client| async move {
            // Account limits are checked with the intended attachment count before any upload.
            client
                .validate_agentic_attachments(&req, args.audio_refs.len(), images.len())
                .await?;
            for id in &args.audio_refs {
                let clip = client
                    .get_clip(id)
                    .await?
                    .ok_or_else(|| CliError::NotFound(format!("clip {id}")))?;
                if clip.id != *id || clip.status != "complete" || clip.is_trashed != Some(false) {
                    return Err(CliError::Config(
                        "audio references must resolve exactly to complete, non-trashed clips"
                            .into(),
                    ));
                }
                let duration = clip
                    .metadata
                    .duration
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .ok_or_else(|| {
                        CliError::Config("audio reference is missing a valid duration".into())
                    })?;
                req.audio_refs.push(AudioReference {
                    clip_id: id.clone(),
                    style_prompt: clip.metadata.tags.as_deref().unwrap_or_default().into(),
                    lyrics: clip.metadata.prompt.as_deref().unwrap_or_default().into(),
                    duration_s: duration,
                });
            }
            client.prepare_generation_request(&mut req).await?;
            for image in images {
                let uploaded = image_upload::run(&client, image).await?;
                req.user_uploaded_image_ids.push(uploaded.upload_id);
            }
            Ok((client, req))
        },
    )
    .await?;
    output_generation(&result, ctx);
    Ok(())
}

async fn submit(
    mut req: GenerateRequest,
    challenge: ChallengeArgs,
    ctx: &AppContext,
) -> Result<(), CliError> {
    let result = execute_generation_submission(
        challenge.token,
        challenge.token_provider,
        ChallengeMode::from_flags(challenge.captcha, challenge.no_captcha),
        ctx,
        move |client| async move {
            client.prepare_generation_request(&mut req).await?;
            Ok((client, req))
        },
    )
    .await?;
    output_generation(&result, ctx);
    Ok(())
}
fn nonempty(value: &str) -> Result<String, CliError> {
    if value.trim().is_empty() {
        Err(CliError::Config("creation input must not be blank".into()))
    } else {
        Ok(value.trim().into())
    }
}
