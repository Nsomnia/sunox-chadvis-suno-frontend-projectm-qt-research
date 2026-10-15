use crate::{
    app::AppContext,
    cli::{AlbumArgs, AlbumCommand},
    core::CliError,
    output,
};
pub async fn run(args: AlbumArgs, ctx: &AppContext) -> Result<(), CliError> {
    let result = match args.command {
        AlbumCommand::List { cursor } => ctx.client().await?.list_albums(cursor.as_deref()).await?,
        AlbumCommand::Info { id } => ctx.client().await?.get_album(&id).await?,
        command => {
            let (client, _guard) = ctx.mutation_client().await?;
            match command {
                AlbumCommand::Create {
                    name,
                    description,
                    r#type,
                    from_playlist,
                } => {
                    client
                        .create_album(
                            &name,
                            &description,
                            r#type.as_str(),
                            from_playlist.as_deref(),
                        )
                        .await?
                }
                AlbumCommand::Set {
                    id,
                    name,
                    description,
                    r#type,
                } => {
                    client
                        .set_album(
                            &id,
                            name.as_deref(),
                            description.as_deref(),
                            r#type.map(|v| v.as_str()),
                        )
                        .await?
                }
                AlbumCommand::Add { id, clip_ids } => {
                    client.add_album_clips(&id, &clip_ids).await?
                }
                AlbumCommand::Publish {
                    id,
                    use_album_cover,
                } => client.publish_album(&id, use_album_cover).await?,
                AlbumCommand::List { .. } | AlbumCommand::Info { .. } => unreachable!(),
            }
        }
    };
    output::json::success(result);
    Ok(())
}
