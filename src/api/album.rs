//! Albums share Playlist storage but have distinct lifecycle and publication contracts.
use super::{SunoClient, mutation::MutationSpec};
use crate::core::CliError;
use serde_json::{Value, json};

impl SunoClient {
    pub async fn list_albums(&self, cursor: Option<&str>) -> Result<Value, CliError> {
        self.with_auth_retry(|| async {
            let mut body = json!({"feed_id":"library_albums","page_size":20});
            if let Some(cursor) = cursor {
                body["cursor"] = json!(cursor);
            }
            // This POST is a read-only feed query, never a mutation.
            let response = self.post("/api/unified/feed").json(&body).send().await?;
            let raw: Value = self.check_response(response).await?.json().await?;
            if !raw.pointer("/feed/items").is_some_and(Value::is_array) {
                return Err(drift("album feed is missing feed.items"));
            }
            Ok(raw)
        })
        .await
    }
    pub async fn get_album(&self, id: &str) -> Result<Value, CliError> {
        let raw = self.album_storage(id).await?;
        if !raw.get("album").is_some_and(Value::is_object) {
            return Err(CliError::Config(
                "the exact resource is not an album".into(),
            ));
        }
        Ok(raw)
    }
    async fn album_storage(&self, id: &str) -> Result<Value, CliError> {
        validate_id(id)?;
        self.with_auth_retry(|| async {
            let raw: Value = self
                .read_json_with_transport_retry(self.get(&format!("/api/playlist/v2/{id}")))
                .await?;
            if raw.pointer("/metadata/id").and_then(Value::as_str) != Some(id) {
                return Err(drift("album storage returned a different or missing ID"));
            }
            Ok(raw)
        })
        .await
    }
    async fn owned_album_storage(&self, id: &str, require_album: bool) -> Result<Value, CliError> {
        let raw = self.album_storage(id).await?;
        if raw
            .pointer("/relationship/is_owned")
            .and_then(Value::as_bool)
            != Some(true)
            || raw
                .pointer("/relationship/can_edit")
                .and_then(Value::as_bool)
                != Some(true)
            || raw
                .pointer("/relationship/is_trashed")
                .and_then(Value::as_bool)
                != Some(false)
        {
            return Err(CliError::Config(
                "album writes require an explicitly owned, editable, non-trashed resource".into(),
            ));
        }
        if require_album && !raw.get("album").is_some_and(Value::is_object) {
            return Err(CliError::Config(
                "the exact resource is not an album".into(),
            ));
        }
        Ok(raw)
    }
    pub async fn create_album(
        &self,
        name: &str,
        description: &str,
        album_type: &str,
        source: Option<&str>,
    ) -> Result<Value, CliError> {
        validate_album_text(Some(name), Some(description))?;
        validate_type(album_type)?;
        if !self.session_info().await?.flag_enabled("can-create-album") {
            return Err(CliError::Config(
                "the current session cannot create albums".into(),
            ));
        }
        if let Some(id) = source {
            let source = self.owned_album_storage(id, false).await?;
            if source.get("album").is_some_and(Value::is_object) {
                return Err(CliError::Config(
                    "--from-playlist requires an ordinary owned playlist".into(),
                ));
            }
        }
        let mut body = json!({"name":name,"description":description,"creation_source":if source.is_some(){"playlist_make_album"}else{"library_albums_tab"},"playlist_type":"album","album_type":album_type,"album_status":"draft"});
        if let Some(id) = source {
            body["source_playlist_id"] = json!(id);
        }
        let spec = MutationSpec::new(
            "album_create",
            "new album draft",
            vec!["sunox album list --json".into()],
        );
        let response: Value = self
            .mutation_json_once(
                self.post_without_redirect("/api/playlist/create/")
                    .json(&body),
                &spec,
            )
            .await?;
        let id = response
            .get("id")
            .or_else(|| response.pointer("/metadata/id"))
            .or_else(|| response.pointer("/playlist/id"))
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| {
                spec.ambiguous(
                    "response_schema",
                    "schema_drift",
                    "album create returned no usable ID".into(),
                )
            })?;
        let raw = self
            .owned_album_storage(id, true)
            .await
            .map_err(|e| spec.with_readback_error(id, e))?;
        if raw.pointer("/bio/description").and_then(Value::as_str) != Some(description)
            || raw.pointer("/album/album_type").and_then(Value::as_str) != Some(album_type)
            || raw.pointer("/metadata/name").and_then(Value::as_str) != Some(name)
            || raw.pointer("/metadata/is_public").and_then(Value::as_bool) != Some(false)
        {
            return Err(spec.with_readback_error(
                id,
                drift("created album draft name or privacy was not confirmed"),
            ));
        }
        Ok(raw)
    }
    pub async fn set_album(
        &self,
        id: &str,
        name: Option<&str>,
        description: Option<&str>,
        album_type: Option<&str>,
    ) -> Result<Value, CliError> {
        validate_album_text(name, description)?;
        if let Some(kind) = album_type {
            validate_type(kind)?;
        }
        if name.is_none() && description.is_none() && album_type.is_none() {
            return Err(CliError::Config("provide an album field to update".into()));
        }
        self.owned_album_storage(id, true).await?;
        let mut body = json!({});
        if let Some(name) = name {
            body["metadata"] = json!({"name":name});
        }
        if let Some(description) = description {
            body["bio"] = json!({"description":description});
        }
        if let Some(kind) = album_type {
            body["album"] = json!({"album_type":kind});
        }
        let spec = album_spec("album_set", id);
        self.send_mutation_once(
            self.patch_without_redirect(&format!("/api/playlist/v2/{id}"))
                .json(&body),
            &spec,
        )
        .await?;
        let raw = self
            .get_album(id)
            .await
            .map_err(|e| spec.with_readback_error(id, e))?;
        if name.is_some_and(|v| raw.pointer("/metadata/name").and_then(Value::as_str) != Some(v))
            || description
                .is_some_and(|v| raw.pointer("/bio/description").and_then(Value::as_str) != Some(v))
            || album_type.is_some_and(|v| {
                raw.pointer("/album/album_type").and_then(Value::as_str) != Some(v)
            })
        {
            return Err(
                spec.with_readback_error(id, drift("updated album fields were not confirmed"))
            );
        }
        Ok(raw)
    }
    pub async fn add_album_clips(&self, id: &str, clip_ids: &[String]) -> Result<Value, CliError> {
        let authenticated_user_id = self.authenticated_user_id().ok_or_else(|| {
            CliError::Config(
                "the authenticated account identity is missing; refusing to add album tracks"
                    .into(),
            )
        })?;
        self.owned_album_storage(id, true).await?;
        for clip_id in clip_ids {
            let clip = self
                .get_clip(clip_id)
                .await?
                .ok_or_else(|| CliError::NotFound(format!("clip {clip_id}")))?;
            if clip.id != *clip_id
                || clip.status != "complete"
                || clip.is_trashed != Some(false)
                || clip.extra.get("user_id").and_then(Value::as_str)
                    != Some(authenticated_user_id.as_str())
            {
                return Err(CliError::Config(
                    "album tracks must be complete, non-trashed and owned by the current account"
                        .into(),
                ));
            }
        }
        self.add_clips_to_playlist(id, clip_ids).await?;
        // The native add endpoint confirms the exact successes and failures.
        Ok(json!({"album_id":id,"added_clip_ids":clip_ids}))
    }
    pub async fn publish_album(&self, id: &str, use_album_cover: bool) -> Result<Value, CliError> {
        if !self.session_info().await?.flag_enabled("can-publish-album") {
            return Err(CliError::Config(
                "the current session cannot publish albums".into(),
            ));
        }
        self.owned_album_storage(id, true).await?;
        let spec = album_spec("album_publish", id);
        let raw: Value = self
            .mutation_json_once(
                self.post_without_redirect(&format!("/api/playlist/v2/{id}/album/publish"))
                    .json(&json!({"use_album_cover":use_album_cover})),
                &spec,
            )
            .await?;
        if raw.get("success").and_then(Value::as_bool) != Some(true)
            || !raw.get("published_clip_ids").is_some_and(|v| {
                v.as_array().is_some_and(|ids| {
                    ids.iter()
                        .all(|id| id.as_str().is_some_and(|id| !id.trim().is_empty()))
                })
            })
        {
            return Err(spec.ambiguous(
                "response_schema",
                "schema_drift",
                "album publish did not confirm success and track IDs; inspect before retrying"
                    .into(),
            ));
        }
        let readback = self
            .get_album(id)
            .await
            .map_err(|e| spec.with_readback_error(id, e))?;
        if readback
            .pointer("/metadata/is_public")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(
                spec.with_readback_error(id, drift("published album visibility was not confirmed"))
            );
        }
        for clip_id in raw["published_clip_ids"].as_array().expect("validated IDs") {
            let clip_id = clip_id.as_str().expect("validated ID");
            let clip = self
                .get_clip(clip_id)
                .await
                .map_err(|e| spec.with_readback_error(id, e))?;
            if !clip.is_some_and(|clip| {
                clip.id == clip_id
                    && clip.extra.get("is_public").and_then(Value::as_bool) == Some(true)
            }) {
                return Err(spec.with_readback_error(
                    id,
                    drift("published track visibility was not confirmed"),
                ));
            }
        }
        Ok(json!({"result":raw,"album":readback}))
    }
}
impl MutationSpec {
    fn with_readback_error(&self, id: &str, error: CliError) -> CliError {
        let mut error = self.ambiguous("readback", error.error_code(), error.to_string());
        if let CliError::AmbiguousMutation { details, .. } = &mut error {
            details["album_id"] = json!(id);
        }
        error
    }
}
fn album_spec(operation: &'static str, id: &str) -> MutationSpec {
    MutationSpec::new(operation, id, vec![format!("sunox album info {id} --json")])
        .with_context("playlist_id", id.to_owned())
}
fn validate_id(id: &str) -> Result<(), CliError> {
    if id.trim().is_empty() || id.contains(['/', '?', '#']) {
        Err(CliError::Config("provide an exact album ID".into()))
    } else {
        Ok(())
    }
}
fn validate_type(kind: &str) -> Result<(), CliError> {
    if matches!(kind, "lp" | "ep") {
        Ok(())
    } else {
        Err(CliError::Config("album type must be lp or ep".into()))
    }
}
fn validate_album_text(name: Option<&str>, description: Option<&str>) -> Result<(), CliError> {
    if name.is_some_and(|v| v.trim().is_empty() || v.chars().count() > 80)
        || description.is_some_and(|v| v.chars().count() > 2000)
    {
        return Err(CliError::Config(
            "album name must be 1..80 characters and description at most 2000".into(),
        ));
    }
    Ok(())
}
fn drift(message: &str) -> CliError {
    CliError::Api {
        code: "schema_drift",
        message: message.into(),
    }
}
