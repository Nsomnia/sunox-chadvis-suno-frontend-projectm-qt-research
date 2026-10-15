use super::VocalGender;
use crate::api::challenge::ChallengeProvider;
use clap::{ArgGroup, Args};
use std::path::PathBuf;

#[derive(Args)]
pub struct ChallengeArgs {
    #[arg(long)]
    pub token: Option<String>,
    #[arg(long, value_enum, requires = "token")]
    pub token_provider: Option<ChallengeProvider>,
    #[arg(long, conflicts_with = "no_captcha")]
    pub captcha: bool,
    #[arg(long, conflicts_with = "captcha")]
    pub no_captcha: bool,
}

/// Speech uses the current v6-mini speech builder, not a music style hint.
#[derive(Args)]
#[command(group(ArgGroup::new("speech_input").required(true).args(["script","script_file","describe"])))]
pub struct SpeechArgs {
    #[arg(long)]
    pub script: Option<String>,
    #[arg(long)]
    pub script_file: Option<PathBuf>,
    #[arg(long)]
    pub describe: Option<String>,
    /// Tone, pacing, mood and setting (Script mode only)
    #[arg(long, conflicts_with = "describe")]
    pub tone: Option<String>,
    #[arg(long, conflicts_with = "describe")]
    pub no_music: bool,
    #[arg(long, value_enum)]
    pub vocal: Option<VocalGender>,
    #[arg(long, conflicts_with = "describe", value_parser=clap::value_parser!(u8).range(0..=4))]
    pub variety: Option<u8>,
    #[command(flatten)]
    pub challenge: ChallengeArgs,
}

#[derive(Args)]
pub struct SimpleArgs {
    /// Natural-language instruction for the current Agentic Simple workflow
    pub prompt: String,
    #[arg(long)]
    pub model: Option<String>,
    /// Existing completed audio clips; repeat to attach multiple references
    #[arg(long = "audio-ref")]
    pub audio_refs: Vec<String>,
    /// Local image attachments, uploaded only after all local and account checks
    #[arg(long = "image-file")]
    pub image_files: Vec<PathBuf>,
    /// Reuse completed, moderated image-upload IDs from a prior invocation
    #[arg(long = "image-id")]
    pub image_ids: Vec<String>,
    #[arg(long)]
    pub lyrics: Option<String>,
    #[arg(long)]
    pub styles: Option<String>,
    #[command(flatten)]
    pub challenge: ChallengeArgs,
}

#[derive(Args)]
pub struct SoundsArgs {
    pub prompt: String,
    #[arg(long)]
    pub model: Option<String>,
    /// Create a loop; otherwise create a one-shot
    #[arg(long)]
    pub r#loop: bool,
    #[arg(long, requires="loop", value_parser=clap::value_parser!(u16).range(1..=300))]
    pub bpm: Option<u16>,
    #[arg(long, requires = "loop")]
    pub key: Option<String>,
    #[command(flatten)]
    pub challenge: ChallengeArgs,
}
