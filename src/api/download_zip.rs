use super::{SunoClient, download::DownloadZipFormat, mutation::MutationSpec};
use crate::core::CliError;
use serde_json::{Value, json};
impl SunoClient {
    pub async fn prepare_download_zip(
        &self,
        ids: &[String],
        workspace: &str,
        format: DownloadZipFormat,
    ) -> Result<Value, CliError> {
        if ids.is_empty()
            || ids.len() > 200
            || ids.iter().any(|id| id.trim().is_empty())
            || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
        {
            return Err(CliError::Config(
                "ZIP requires 1..200 distinct clip IDs".into(),
            ));
        }
        let batch_id = uuid::Uuid::new_v4().to_string();
        let spec = MutationSpec::new(
            "download_zip_prepare",
            "ZIP batch",
            vec!["sunox credits --json".into()],
        )
        .with_context("clip_ids", json!(ids))
        .with_context("batch_id", json!(batch_id));
        let mut raw:Value=self.mutation_json_once(self.post_without_redirect("/api/download/clips/zip/prepare").json(&json!({"clip_ids":ids,"workspace_name":workspace,"format":format.extension(),"batch_id":batch_id})),&spec).await?;
        validate_reported_results(&raw, &spec)?;
        // A valid URL can contain the successful subset. Preserve failures for
        // the caller's partial-success warning instead of discarding that archive.
        let url = raw
            .get("download_url")
            .and_then(Value::as_str)
            .filter(|url| url::is_https(url));
        if url.is_none() {
            if let Some(failures) = raw
                .get("failed_clips")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
            {
                return Err(CliError::PartialMutation {
                    message: "ZIP preparation failed without a usable archive; inspect the batch before retrying".into(),
                    details: json!({"operation":"download_zip_prepare", "batch_id":batch_id,
                        "failed_clips":failures, "clip_ids":ids, "recovery":{"resumable":false}}),
                });
            }
            return Err(spec.ambiguous(
                "response_schema",
                "schema_drift",
                "ZIP response did not contain a valid HTTPS download URL".into(),
            ));
        }
        raw["requested_batch_id"] = json!(batch_id);
        Ok(raw)
    }
}

fn validate_reported_results(raw: &Value, spec: &MutationSpec) -> Result<(), CliError> {
    for field in ["failed_clips", "substituted_clips"] {
        let Some(value) = raw.get(field).filter(|value| !value.is_null()) else {
            continue;
        };
        let entries = value.as_array().ok_or_else(|| {
            spec.ambiguous(
                "response_schema",
                "schema_drift",
                format!("ZIP response {field} must be an array or null"),
            )
        })?;
        if field == "substituted_clips"
            && entries.iter().any(|entry| {
                !entry.is_object()
                    || ["requested_format", "actual_format"].iter().any(|key| {
                        entry
                            .get(key)
                            .and_then(Value::as_str)
                            .is_none_or(|format| format.trim().is_empty())
                    })
            })
        {
            return Err(spec.ambiguous(
                "response_schema",
                "schema_drift",
                "ZIP substitutions must identify the requested and actual formats".into(),
            ));
        }
    }
    Ok(())
}

// Never accept a script/data/file URL as a download destination. Media requests carry no auth.
mod url {
    pub fn is_https(value: &str) -> bool {
        reqwest::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_result_lists_accept_native_nullish_values_and_preserve_substitutions() {
        let spec = MutationSpec::new("download_zip_prepare", "ZIP batch", Vec::new());
        for raw in [
            json!({}),
            json!({"failed_clips": null, "substituted_clips": null}),
            json!({"failed_clips": [], "substituted_clips": []}),
            json!({"substituted_clips": [{"clip_id": "clip-a", "requested_format": "m4a", "actual_format": "mp3"}]}),
        ] {
            let original = raw.clone();
            validate_reported_results(&raw, &spec).unwrap();
            assert_eq!(raw, original);
        }
    }

    #[test]
    fn malformed_zip_result_lists_are_ambiguous_after_submission() {
        let spec = MutationSpec::new("download_zip_prepare", "ZIP batch", Vec::new());
        for raw in [
            json!({"failed_clips": {}}),
            json!({"substituted_clips": false}),
            json!({"substituted_clips": ["clip-a"]}),
            json!({"substituted_clips": [{"requested_format": "m4a"}]}),
            json!({"substituted_clips": [{"requested_format": "m4a", "actual_format": " "}]}),
        ] {
            assert_eq!(
                validate_reported_results(&raw, &spec)
                    .unwrap_err()
                    .error_code(),
                "ambiguous_mutation"
            );
        }
    }
}
