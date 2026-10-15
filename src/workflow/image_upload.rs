use std::path::Path;

use serde::Serialize;

use crate::api::SunoClient;
use crate::api::types::{CreateImageUploadRequest, SetMetadataRequest};
use crate::core::CliError;

use super::upload::upload_filename;

#[derive(Debug, Serialize)]
pub struct ImageUploadResult {
    pub upload_id: String,
    pub image_url: String,
    pub cover_image_s3_id: String,
    pub moderation_status: Option<String>,
}

// Local memory safety limit, not a claim about Suno's upload allowance.
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
// Current Web image attachments validate a 10 MiB limit before initiation.
const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;

pub struct PreparedImage {
    extension: String,
    filename: String,
    bytes: Vec<u8>,
}

impl PreparedImage {
    pub(crate) fn byte_len(&self) -> usize {
        self.bytes.len()
    }
}

pub async fn prepare(file: &Path) -> Result<PreparedImage, CliError> {
    let extension = image_extension(file)?;
    prepare_with_limit(file, extension, MAX_IMAGE_BYTES).await
}

pub(crate) async fn prepare_attachment(file: &Path) -> Result<PreparedImage, CliError> {
    let extension = file
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|extension| matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp"))
        .ok_or_else(|| {
            CliError::Config("image attachments must be png, jpg, jpeg, webp, or bmp".into())
        })?;
    let mut image = prepare_with_limit(file, extension, MAX_ATTACHMENT_BYTES).await?;
    image.extension = attachment_content_extension(&image.bytes)
        .ok_or_else(|| {
            CliError::Config("image attachment contents are not a supported image".into())
        })?
        .into();
    Ok(image)
}

fn attachment_content_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        Some("webp")
    } else if bytes.starts_with(b"BM") {
        Some("bmp")
    } else {
        None
    }
}

async fn prepare_with_limit(
    file: &Path,
    extension: String,
    max_bytes: u64,
) -> Result<PreparedImage, CliError> {
    use tokio::io::AsyncReadExt;

    let filename = upload_filename(file)?;
    let metadata = tokio::fs::metadata(file).await?;
    if !metadata.is_file() {
        return Err(CliError::Config(
            "image upload path must be a regular file".into(),
        ));
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let source = options.open(file).await?;
    let metadata = source.metadata().await?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max_bytes {
        return Err(CliError::Config(format!(
            "image upload must be a nonempty regular file within the {max_bytes}-byte limit"
        )));
    }
    let mut bytes = Vec::new();
    source.take(max_bytes + 1).read_to_end(&mut bytes).await?;
    if bytes.is_empty() || bytes.len() as u64 > max_bytes {
        return Err(CliError::Config(format!(
            "image upload must be nonempty and within the {max_bytes}-byte limit"
        )));
    }
    Ok(PreparedImage {
        extension,
        filename,
        bytes,
    })
}

pub async fn run(client: &SunoClient, image: PreparedImage) -> Result<ImageUploadResult, CliError> {
    let PreparedImage {
        extension,
        filename,
        bytes,
    } = image;
    let upload = client
        .create_image_upload(&CreateImageUploadRequest { extension })
        .await?;
    let mut completed_steps = vec!["upload_created"];
    let content_type = upload_content_type(&upload.fields);

    client
        .upload_presigned_image_form(&upload.url, &upload.fields, &filename, content_type, bytes)
        .await
        .map_err(|error| {
            image_upload_stage_error(&upload.id, &completed_steps, "file_upload", error)
        })?;
    completed_steps.push("file_uploaded");

    let finish = client
        .finish_image_upload(&upload.id)
        .await
        .map_err(|error| {
            image_upload_stage_error(&upload.id, &completed_steps, "upload_finish", error)
        })?;
    completed_steps.push("upload_finished");
    if finish.moderation_status.as_deref() != Some("approved") {
        return Err(image_upload_stage_error(
            &upload.id,
            &completed_steps,
            "moderation",
            CliError::Api {
                code: "image_moderation",
                message: format!(
                    "image upload {} was not approved by Suno moderation: {}",
                    upload.id,
                    finish.moderation_status.as_deref().unwrap_or("unknown")
                ),
            },
        ));
    }

    let cover_image_s3_id = format!("image_{}", upload.id);
    Ok(ImageUploadResult {
        upload_id: upload.id,
        image_url: format!("https://cdn2.suno.ai/{cover_image_s3_id}.jpeg"),
        cover_image_s3_id,
        moderation_status: finish.moderation_status,
    })
}

pub async fn apply_uploaded_cover_to_clip(
    client: &SunoClient,
    clip_id: &str,
    request: &SetMetadataRequest,
    cover: &ImageUploadResult,
) -> Result<(), CliError> {
    client
        .set_metadata(clip_id, request)
        .await
        .map_err(|error| {
            if matches!(error, CliError::AmbiguousMutation { .. }) {
                return enrich_image_ambiguity(
                    error,
                    clip_id,
                    &["cover_uploaded"],
                    "metadata_update",
                    Some(cover),
                );
            }
            let failed = mutation_failure("metadata_update", &error);
            CliError::PartialMutation {
                message: format!(
                    "clip_set for {clip_id} stopped at metadata_update after 1 completed step"
                ),
                details: serde_json::json!({
                "operation": "clip_set",
                "clip_id": clip_id,
                "cover": {
                    "upload_id": cover.upload_id,
                    "image_url": cover.image_url,
                    "uploaded_here": true
                },
                "completed_steps": ["cover_uploaded"],
                "failed": failed,
                "recovery": {
                    "resumable": true,
                    "command": "sunox clip set",
                    "arguments": {
                        "clip_id": clip_id,
                        "image_url": cover.image_url
                    },
                    "reuse_original_arguments": true,
                    "omit_original_arguments": ["image_file"]
                }
                }),
            }
        })
}

fn image_upload_stage_error(
    upload_id: &str,
    completed_steps: &[&str],
    failed_step: &str,
    error: CliError,
) -> CliError {
    if matches!(error, CliError::AmbiguousMutation { .. }) {
        return enrich_image_ambiguity(error, upload_id, completed_steps, failed_step, None);
    }
    let recovery = match failed_step {
        "file_upload" => serde_json::json!({
            "resumable": false,
            "reason": "the presigned image upload form cannot be safely reconstructed"
        }),
        "upload_finish" => serde_json::json!({
            "resumable": false,
            "reason": "retry safety for image upload finish is not live-verified"
        }),
        "moderation" => serde_json::json!({
            "resumable": false,
            "reason": "the uploaded image was not approved"
        }),
        _ => serde_json::json!({ "resumable": false }),
    };
    let failed = mutation_failure(failed_step, &error);
    CliError::PartialMutation {
        message: format!(
            "image upload {upload_id} stopped at {failed_step} after {} completed step(s)",
            completed_steps.len()
        ),
        details: serde_json::json!({
            "operation": "image_upload",
            "upload_id": upload_id,
            "completed_steps": completed_steps,
            "failed": failed,
            "recovery": recovery
        }),
    }
}

fn enrich_image_ambiguity(
    mut error: CliError,
    resource_id: &str,
    completed_steps: &[&str],
    failed_step: &str,
    cover: Option<&ImageUploadResult>,
) -> CliError {
    if let CliError::AmbiguousMutation { details, .. } = &mut error
        && let Some(fields) = details.as_object_mut()
    {
        fields.insert("resource_id".into(), serde_json::json!(resource_id));
        fields.insert("completed_steps".into(), serde_json::json!(completed_steps));
        fields.insert("failed_step".into(), serde_json::json!(failed_step));
        if let Some(cover) = cover {
            fields.insert(
                "cover".into(),
                serde_json::json!({
                    "upload_id": cover.upload_id,
                    "image_url": cover.image_url,
                    "uploaded_here": true
                }),
            );
        }
    }
    error
}

fn mutation_failure(step: &str, error: &CliError) -> serde_json::Value {
    let mut failed = serde_json::json!({
        "step": step,
        "code": error.error_code(),
        "message": error.to_string()
    });
    if let Some(details) = error.details() {
        failed["details"] = details.clone();
    }
    failed
}

pub fn image_extension(path: &Path) -> Result<String, CliError> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.trim_start_matches('.').to_ascii_lowercase())
        .filter(|extension| matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp"))
        .ok_or_else(|| {
            CliError::Config("image upload file must be png, jpg, jpeg, or webp".into())
        })?;
    Ok(extension)
}

fn upload_content_type(fields: &std::collections::BTreeMap<String, String>) -> Option<&str> {
    fields
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use std::collections::BTreeMap;

    use super::{image_extension, upload_content_type};

    #[test]
    fn image_extension_accepts_supported_images() {
        assert_eq!(
            image_extension(Path::new("/tmp/Cover.PNG")).expect("extension"),
            "png"
        );
        assert_eq!(
            image_extension(Path::new("/tmp/Cover.jpeg")).expect("extension"),
            "jpeg"
        );
    }

    #[test]
    fn image_extension_rejects_non_images() {
        let err = image_extension(Path::new("/tmp/Cover.txt")).expect_err("image extension");

        assert!(err.to_string().contains("png, jpg, jpeg, or webp"));
    }

    #[test]
    fn upload_content_type_reads_case_insensitive_s3_field() {
        let mut fields = BTreeMap::new();
        fields.insert("content-type".to_string(), "image/png".to_string());

        assert_eq!(upload_content_type(&fields), Some("image/png"));
    }

    #[tokio::test]
    async fn image_attachment_derives_upload_type_from_contents_and_accepts_bmp() {
        let directory = tempfile::tempdir().unwrap();
        for (name, bytes, expected) in [
            ("renamed.jpg", b"\x89PNG\r\n\x1a\n".as_slice(), "png"),
            ("photo.jpg", b"\xff\xd8\xff".as_slice(), "jpeg"),
            ("picture.bmp", b"BM".as_slice(), "bmp"),
            ("picture.webp", b"RIFF1234WEBP".as_slice(), "webp"),
        ] {
            let path = directory.path().join(name);
            tokio::fs::write(&path, bytes).await.unwrap();
            let prepared = super::prepare_attachment(&path).await.unwrap();
            assert_eq!(prepared.extension, expected);
        }
    }

    #[tokio::test]
    async fn image_attachment_rejects_renamed_text_and_oversize_before_upload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.png");
        tokio::fs::write(&path, b"not an image").await.unwrap();
        assert!(
            super::prepare_attachment(&path)
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("contents")
        );
        let file = tokio::fs::File::create(&path).await.unwrap();
        file.set_len(super::MAX_ATTACHMENT_BYTES + 1).await.unwrap();
        assert!(
            super::prepare_attachment(&path)
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("10485760-byte")
        );
    }
}
