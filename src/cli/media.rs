pub use crate::api::download::{DownloadFormat, DownloadZipFormat};

#[derive(clap::Args)]
pub struct UploadArgs {
    /// Local audio file to upload
    pub file: String,

    /// Suno upload type value
    #[arg(long, default_value = "file_upload")]
    pub upload_type: String,

    /// Mark the uploaded audio as a stem mix
    #[arg(long)]
    pub stem_mix: bool,

    /// Optional clip title to set after initialization
    #[arg(short, long)]
    pub title: Option<String>,

    /// Optional lyrics to set after initialization
    #[arg(long, conflicts_with = "lyrics_file")]
    pub lyrics: Option<String>,

    /// Read optional lyrics from a file
    #[arg(long)]
    pub lyrics_file: Option<String>,

    /// Max wait time for Suno upload processing, in seconds
    #[arg(long)]
    pub timeout: Option<u64>,
}

#[derive(clap::Args)]
pub struct UploadStatusArgs {
    /// Suno audio upload ID
    pub upload_id: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum DownloadRoute {
    /// Suno export; may consume download allowance
    Official,
    /// Exact unencrypted audio URL returned by Suno; never authorizes or converts
    Cdn,
    /// Reuse or generate a song video, then copy its original audio into M4A
    #[default]
    Video,
}

#[derive(clap::Args)]
pub struct DownloadArgs {
    /// Clip ID(s); the official source authorizes locked songs unless --read-only is set
    pub ids: Vec<String>,

    /// Download source; CDN/video never authorize or fall back to official export
    #[arg(long, value_enum, default_value = "video")]
    pub source: DownloadRoute,

    /// Compatibility flag: video source already generates missing/stale videos unless --read-only
    #[arg(long)]
    pub generate_video: bool,

    /// Maximum seconds to wait when generating a missing song video
    #[arg(long)]
    pub video_timeout: Option<u64>,

    /// Output directory
    #[arg(short, long)]
    pub output: Option<String>,

    /// Replace an existing downloaded file with the same clip ID and format
    #[arg(long)]
    pub force: bool,

    /// Save MP4 instead of audio (official prepared export, or direct video source)
    #[arg(long)]
    pub video: bool,

    /// Audio format (official: MP3 default; CDN: existing M4A/MP3; video: unchanged M4A only)
    #[arg(long, value_enum)]
    pub format: Option<DownloadFormat>,

    /// Refuse legacy server-side WAV conversion when no converted file exists
    #[arg(long, conflicts_with = "video")]
    pub no_convert: bool,

    /// Internal safety switch for media such as stems that must never trigger
    /// aligned-lyrics generation while downloading an MP3.
    #[arg(skip)]
    pub skip_timed_lyrics: bool,
}

#[derive(clap::Args)]
pub struct TimedLyricsArgs {
    /// Clip ID
    pub id: String,

    /// Output as LRC format
    #[arg(long)]
    pub lrc: bool,
}

#[derive(clap::Args)]
pub struct DownloadZipArgs {
    #[arg(required=true, num_args=1..=200)]
    pub ids: Vec<String>,
    #[arg(long, default_value = "m4a", value_enum)]
    pub format: DownloadZipFormat,
    #[arg(long, default_value = "Suno songs")]
    pub workspace_name: String,
    #[arg(short, long)]
    pub output: Option<String>,
    #[arg(long)]
    pub force: bool,
}
