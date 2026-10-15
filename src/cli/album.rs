use clap::{Args, Subcommand, ValueEnum};
#[derive(Args)]
pub struct AlbumArgs {
    #[command(subcommand)]
    pub command: AlbumCommand,
}
#[derive(ValueEnum, Clone, Copy)]
pub enum AlbumType {
    Lp,
    Ep,
}
impl AlbumType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lp => "lp",
            Self::Ep => "ep",
        }
    }
}
#[derive(Subcommand)]
pub enum AlbumCommand {
    /// Read the album library (one page; reuse the returned cursor)
    List {
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read the exact album's v2 metadata
    Info { id: String },
    /// Create a private draft, optionally initialized from an owned playlist
    Create {
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, value_enum, default_value = "lp")]
        r#type: AlbumType,
        #[arg(long)]
        from_playlist: Option<String>,
    },
    /// Update an owned album's name, description or type
    Set {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, value_enum)]
        r#type: Option<AlbumType>,
    },
    /// Add completed clips to an owned album
    Add {
        id: String,
        #[arg(required = true)]
        clip_ids: Vec<String>,
    },
    /// Explicitly publish an album and its eligible tracks; may notify followers
    Publish {
        id: String,
        #[arg(long)]
        use_album_cover: bool,
    },
}
