//! Optional local tools for inspecting song-video quality and copying its audio.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::download;
use crate::api::types::Clip;
use crate::core::CliError;

const TOOL_TIMEOUT: Duration = Duration::from_secs(120);
const TOOL_OUTPUT_LIMIT: u64 = 64 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StreamQuality {
    pub codec_type: String,
    pub codec_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_integer",
        skip_serializing_if = "Option::is_none"
    )]
    pub bit_rate: Option<u64>,
    #[serde(
        default,
        deserialize_with = "optional_integer",
        skip_serializing_if = "Option::is_none"
    )]
    pub sample_rate: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_frame_rate: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContainerQuality {
    pub format_name: String,
    pub duration: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_integer",
        skip_serializing_if = "Option::is_none"
    )]
    pub bit_rate: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediaQuality {
    pub streams: Vec<StreamQuality>,
    pub format: ContainerQuality,
}

impl MediaQuality {
    pub fn audio(&self) -> Option<&StreamQuality> {
        self.streams
            .iter()
            .find(|stream| stream.codec_type == "audio")
    }
}

fn optional_integer<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(serde_json::Value::String(text)) if text == "N/A" => Ok(None),
        Some(serde_json::Value::String(text)) => {
            text.parse().map(Some).map_err(serde::de::Error::custom)
        }
        Some(serde_json::Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("expected unsigned integer")),
        _ => Err(serde::de::Error::custom(
            "expected numeric string or unsigned integer",
        )),
    }
}

pub struct VideoTools {
    ffprobe: PathBuf,
    ffmpeg: Option<PathBuf>,
}

impl VideoTools {
    /// Verify optional dependencies before any account write or transfer.
    pub async fn discover(extract_audio: bool) -> Result<Self, CliError> {
        let tools = Self {
            ffprobe: PathBuf::from("ffprobe"),
            ffmpeg: extract_audio.then(|| PathBuf::from("ffmpeg")),
        };
        for program in std::iter::once(&tools.ffprobe).chain(tools.ffmpeg.iter()) {
            run_tool(
                program,
                &[OsString::from("-version")],
                Duration::from_secs(10),
            )
            .await?;
        }
        Ok(tools)
    }

    pub async fn inspect(&self, path: &Path) -> Result<MediaQuality, CliError> {
        let mut args = words(&[
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-show_entries",
            "format=format_name,duration,bit_rate:stream=codec_type,codec_name,profile,bit_rate,sample_rate,channels,width,height,avg_frame_rate",
            "-of",
            "json",
        ]);
        args.push(path.as_os_str().to_owned());
        let data = run_tool(&self.ffprobe, &args, TOOL_TIMEOUT).await?;
        let quality: MediaQuality = serde_json::from_slice(&data).map_err(|error| {
            CliError::Download(format!("invalid ffprobe media report: {error}"))
        })?;
        if quality.streams.is_empty() || quality.streams.len() > 64 || quality.audio().is_none() {
            return Err(CliError::Download(
                "song video must contain an inspectable audio stream".into(),
            ));
        }
        Ok(quality)
    }

    pub async fn extract_audio(
        &self,
        clip: &Clip,
        source: &Path,
        source_quality: &MediaQuality,
        directory: &str,
        force: bool,
    ) -> Result<(String, MediaQuality), CliError> {
        let program = self
            .ffmpeg
            .as_ref()
            .ok_or_else(|| CliError::Config("audio extraction requires FFmpeg".into()))?;
        let original = source_quality
            .audio()
            .ok_or_else(|| CliError::Download("video has no audio stream".into()))?;
        if !matches!(original.codec_name.as_str(), "aac" | "opus") {
            return Err(CliError::Download(format!(
                "video audio codec {} is not supported for unchanged M4A extraction",
                original.codec_name
            )));
        }
        let staged = download::stage_local_output(clip, directory, "m4a", force).await?;
        let mut args = words(&[
            "-nostdin",
            "-v",
            "error",
            "-xerror",
            "-y",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
        ]);
        args.push(source.as_os_str().to_owned());
        args.extend(words(&[
            "-map",
            "0:a:0",
            "-vn",
            "-c:a",
            "copy",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
        ]));
        args.push(staged.path().as_os_str().to_owned());
        run_tool(program, &args, TOOL_TIMEOUT).await?;
        let quality = self.inspect(staged.path()).await?;
        let audio = quality.audio().expect("inspection requires audio");
        if quality.streams.len() != 1
            || audio.codec_name != original.codec_name
            || audio.sample_rate != original.sample_rate
            || audio.channels != original.channels
        {
            return Err(CliError::Download(
                "audio extraction changed the source stream properties".into(),
            ));
        }
        if tokio::fs::metadata(staged.path()).await?.len() > 2 * 1024 * 1024 * 1024 {
            return Err(CliError::Download(
                "extracted audio exceeds the 2 GiB safety limit".into(),
            ));
        }
        let path = staged.validate_and_commit("m4a").await?;
        Ok((path, quality))
    }
}

fn words(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

async fn run_tool(
    program: &Path,
    args: &[OsString],
    timeout: Duration,
) -> Result<Vec<u8>, CliError> {
    let mut child = Command::new(program).args(args).stdin(Stdio::null())
        .stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn()
        .map_err(|error| CliError::Config(format!(
            "cannot start {}: {error}. Install FFmpeg (ffmpeg and ffprobe) for --source video audio extraction",
            program.display()
        )))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let work = async {
        let (output, errors, status) =
            tokio::join!(read_tool_pipe(stdout), read_tool_pipe(stderr), child.wait());
        let output = output?;
        let errors = errors?;
        let status = status?;
        if output.len() as u64 > TOOL_OUTPUT_LIMIT || errors.len() as u64 > TOOL_OUTPUT_LIMIT {
            return Err(CliError::Download(
                "media tool output exceeded its 64 KiB safety limit".into(),
            ));
        }
        if !status.success() {
            return Err(CliError::Download(format!(
                "{} failed: {}",
                program.display(),
                String::from_utf8_lossy(&errors).trim()
            )));
        }
        Ok(output)
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| CliError::Download(format!("{} exceeded its time limit", program.display())))?
}

async fn read_tool_pipe(reader: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader
        .take(TOOL_OUTPUT_LIMIT + 1)
        .read_to_end(&mut output)
        .await?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_reported_quality_without_inventing_missing_bitrate() {
        let q: MediaQuality = serde_json::from_value(serde_json::json!({
            "streams":[{"codec_type":"audio","codec_name":"aac","profile":"LC","bit_rate":"191977","sample_rate":"48000","channels":2}],
            "format":{"format_name":"mov,mp4,m4a","duration":"19.605"}
        })).unwrap();
        assert_eq!(q.audio().unwrap().bit_rate, Some(191977));
        assert_eq!(q.audio().unwrap().sample_rate, Some(48000));
        assert_eq!(q.format.bit_rate, None);
        let mut bad = serde_json::to_value(q).unwrap();
        bad["streams"][0]["bit_rate"] = serde_json::json!("invalid");
        assert!(serde_json::from_value::<MediaQuality>(bad).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tool_failure_timeout_and_oversized_output_are_bounded() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        for (name, script, timeout) in [
            (
                "failure",
                "#!/bin/sh\necho broken >&2\nexit 1\n",
                Duration::from_secs(2),
            ),
            (
                "timeout",
                "#!/bin/sh\nexec /bin/sleep 10\n",
                Duration::from_millis(30),
            ),
            (
                "oversize",
                "#!/bin/sh\n/usr/bin/head -c 70000 /dev/zero\n",
                Duration::from_secs(2),
            ),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert!(run_tool(&path, &[], timeout).await.is_err());
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn extraction_uses_copy_and_atomically_preserves_existing_output_on_invalid_media() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("fixture with spaces.m4a");
        std::fs::write(
            &fixture,
            include_bytes!("../../tests/fixtures/download/silence.m4a"),
        )
        .unwrap();
        let report = r#"{"streams":[{"codec_type":"audio","codec_name":"aac","sample_rate":"44100","channels":1}],"format":{"format_name":"mov,mp4,m4a","duration":"0.1"}}"#;
        let ffprobe = dir.path().join("probe");
        std::fs::write(&ffprobe, format!("#!/bin/sh\nprintf '%s' '{report}'\n")).unwrap();
        std::fs::set_permissions(&ffprobe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let ffmpeg = dir.path().join("remux");
        let args_file = dir.path().join("arguments");
        let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nfor last do :; done\n/bin/cp {} \"$last\"\n",
            quote(&args_file),
            quote(&fixture)
        );
        std::fs::write(&ffmpeg, script).unwrap();
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tools = VideoTools {
            ffprobe,
            ffmpeg: Some(ffmpeg.clone()),
        };
        let clip:Clip=serde_json::from_value(serde_json::json!({"id":"clip-1","title":"Track","status":"complete","model_name":"test","created_at":""})).unwrap();
        let source_quality: MediaQuality = serde_json::from_str(report).unwrap();
        let (path, _) = tools
            .extract_audio(
                &clip,
                &fixture,
                &source_quality,
                dir.path().to_str().unwrap(),
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            std::fs::read(&fixture).unwrap()
        );
        let arguments = std::fs::read_to_string(args_file).unwrap();
        assert!(arguments.contains("-c:a\ncopy\n"));
        assert!(arguments.contains("-vn\n"));
        std::fs::write(&path, b"preserve this file").unwrap();
        std::fs::write(
            &ffmpeg,
            "#!/bin/sh\nfor last do :; done\nprintf broken > \"$last\"\n",
        )
        .unwrap();
        assert!(
            tools
                .extract_audio(
                    &clip,
                    &fixture,
                    &source_quality,
                    dir.path().to_str().unwrap(),
                    true
                )
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), b"preserve this file");
    }
}
