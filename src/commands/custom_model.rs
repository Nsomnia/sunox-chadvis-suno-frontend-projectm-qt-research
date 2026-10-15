use std::collections::BTreeSet;

use serde_json::json;

use crate::app::AppContext;
use crate::cli::{CustomModelCommand, CustomModelsArgs};
use crate::core::{CliError, ensure_destructive_confirmed};
use crate::output::{self, OutputFormat};

pub async fn run(args: CustomModelsArgs, ctx: &AppContext) -> Result<(), CliError> {
    match args.command {
        CustomModelCommand::Bases => {
            let data = ctx.client().await?.custom_model_bases().await?;
            match ctx.fmt {
                OutputFormat::Json => output::json::success(data),
                OutputFormat::Table => {
                    for base in data.options {
                        println!(
                            "{}\t{}\tdefault={}",
                            base.base_model, base.name, base.is_default
                        );
                    }
                }
            }
            Ok(())
        }
        CustomModelCommand::Shared { cursor } => {
            let data = ctx
                .client()
                .await?
                .shared_custom_models(cursor.as_deref())
                .await?;
            output::json::success(data);
            Ok(())
        }
        CustomModelCommand::Rename { id, name } => {
            let (client, _guard) = ctx.mutation_client().await?;
            client.rename_custom_model(&id, &name).await?;
            output::json::success(json!({"model_id":id,"name":name}));
            Ok(())
        }
        CustomModelCommand::Pending => pending(ctx).await,
        CustomModelCommand::Train(args) => {
            let confirm_ui_available = args.confirm_ui_available;
            let (clip_ids, name) = validate_training(
                args.clip_ids,
                &args.name,
                args.confirm_rights,
                confirm_ui_available,
            )?;
            let (client, _mutation_guard) = ctx.mutation_client().await?;
            if !ctx.quiet {
                eprintln!(
                    "Custom Model training currently costs 100 credits; validating the live account entitlement and {} source clips with the current training base and session gate...",
                    clip_ids.len()
                );
            }
            let model = client
                .create_custom_model(
                    &clip_ids,
                    &name,
                    confirm_ui_available,
                    args.base_model.as_deref(),
                )
                .await?;
            match ctx.fmt {
                OutputFormat::Json => output::json::success(model),
                OutputFormat::Table => {
                    println!("Custom Model training started: {} ({name})", model.id)
                }
            }
            Ok(())
        }
        CustomModelCommand::Archive(args) => {
            ensure_destructive_confirmed(args.yes, "sunox models custom archive")?;
            let model_id = nonempty("Custom Model ID", args.id)?;
            let (client, _mutation_guard) = ctx.mutation_client().await?;
            client.archive_custom_model(&model_id).await?;
            match ctx.fmt {
                OutputFormat::Json => output::json::success(json!({
                    "model_id": model_id,
                    "archived": true,
                })),
                OutputFormat::Table => println!("Archived Custom Model {model_id}"),
            }
            Ok(())
        }
    }
}

async fn pending(ctx: &AppContext) -> Result<(), CliError> {
    let response = ctx.client().await?.pending_custom_models().await?;
    match ctx.fmt {
        OutputFormat::Json => output::json::success(response),
        OutputFormat::Table => {
            if response.pending_models.is_empty() {
                println!("No Custom Models are training");
            } else {
                for model in response.pending_models {
                    println!("{}\t{}", model.id, model.name);
                }
            }
        }
    }
    Ok(())
}

fn validate_training(
    clip_ids: Vec<String>,
    name: &str,
    confirm_rights: bool,
    _confirm_ui_available: bool,
) -> Result<(Vec<String>, String), CliError> {
    if !confirm_rights {
        return Err(CliError::Config(
            "Custom Model training requires --confirm-rights to affirm ownership of every source clip"
                .into(),
        ));
    }
    let name = nonempty("Custom Model name", name.to_string())?;
    if name.chars().count() > 16 {
        return Err(CliError::Config(
            "Custom Model name must not exceed the current Web limit of 16 Unicode characters"
                .into(),
        ));
    }
    let mut normalized = Vec::with_capacity(clip_ids.len());
    let mut unique = BTreeSet::new();
    for clip_id in clip_ids {
        let clip_id = nonempty("clip ID", clip_id)?;
        if !unique.insert(clip_id.clone()) {
            return Err(CliError::Config(format!(
                "Custom Model source clip `{clip_id}` is duplicated; provide at least 6 distinct clip IDs"
            )));
        }
        normalized.push(clip_id);
    }
    if normalized.len() < 6 {
        return Err(CliError::Config(
            "Custom Model training requires at least 6 distinct clip IDs".into(),
        ));
    }
    if normalized.len() > 100 {
        return Err(CliError::Config(
            "Custom Model training accepts at most 100 source clips in this CLI; Artist accounts can submit larger sets in Suno Web"
                .into(),
        ));
    }
    Ok((normalized, name))
}

fn nonempty(label: &str, value: String) -> Result<String, CliError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(CliError::Config(format!("{label} must not be empty")));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::validate_training;

    #[test]
    fn training_requires_six_unique_clips_and_rights_confirmation() {
        let six = (1..=6).map(|index| format!("clip-{index}")).collect();
        validate_training(six, "My Sound", true, true).expect("valid training request");

        let duplicate = vec!["clip-1".to_string(); 6];
        assert!(validate_training(duplicate, "My Sound", true, true).is_err());

        let six = (1..=6).map(|index| format!("clip-{index}")).collect();
        assert!(validate_training(six, "My Sound", false, true).is_err());

        let six = (1..=6).map(|index| format!("clip-{index}")).collect();
        assert!(validate_training(six, "My Sound", true, false).is_ok());

        let six = (1..=6).map(|index| format!("clip-{index}")).collect();
        assert!(validate_training(six, &"名".repeat(17), true, true).is_err());
    }
}
