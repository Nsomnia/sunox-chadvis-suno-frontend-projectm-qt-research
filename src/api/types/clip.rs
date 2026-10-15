use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Clip {
    pub id: String,
    pub title: String,
    pub status: String,
    pub model_name: String,
    pub audio_url: Option<String>,
    pub video_url: Option<String>,
    pub image_url: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_trashed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_download_unlocked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_config: Option<ClipActionConfig>,
    #[serde(default)]
    pub play_count: u64,
    #[serde(default)]
    pub upvote_count: u64,
    #[serde(default)]
    pub metadata: ClipMetadata,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Clip {
    /// Return the non-authorizing URL that the current Web player can stream.
    ///
    /// Suno may expose `/api/forbidden` in the legacy top-level `audio_url`
    /// while providing an encoded progressive M4A in `media_urls`. The Web
    /// player decrypts those separately; native playback must only select
    /// unencoded media. Direct MP3 media URLs are deliberately not selected
    /// here because they may require the separate download workflow.
    pub fn playback_url(&self) -> Option<&str> {
        self.audio_url
            .as_deref()
            .filter(|url| is_usable_audio_url(url) && !self.is_encoded_media_url(url))
            .or_else(|| self.progressive_m4a_url())
    }

    /// Select an exact, unencoded URL returned by Suno for a requested format.
    ///
    /// This does not authorize a download, prepare a file, rewrite a CDN URL,
    /// or guarantee CDN access. A protected URL may still return HTTP 403.
    pub fn direct_audio_url(&self, extension: &str) -> Option<&str> {
        if !matches!(extension, "mp3" | "m4a") {
            return None;
        }
        self.audio_url
            .as_deref()
            .filter(|url| {
                is_direct_audio_url(url)
                    && !self.is_encoded_media_url(url)
                    && reqwest::Url::parse(url).ok().is_some_and(|url| {
                        url.path()
                            .rsplit_once('.')
                            .is_some_and(|(_, actual)| actual.eq_ignore_ascii_case(extension))
                    })
            })
            .or_else(|| {
                self.extra
                    .get("media_urls")?
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_object)
                    .find_map(|media| {
                        let url = media.get("url")?.as_str()?;
                        (is_unencoded_media(media)
                            && !self.is_encoded_media_url(url)
                            && media.get("delivery")?.as_str()? == "progressive"
                            && media_extension(media.get("content_type")?.as_str()?)
                                == Some(extension)
                            && is_direct_audio_url(url))
                        .then_some(url)
                    })
            })
    }

    /// Exact unencrypted video URL, only when the clip does not mark it stale.
    pub fn direct_video_url(&self) -> Option<&str> {
        if self
            .metadata
            .extra
            .get("video_is_stale")
            .is_some_and(|value| !matches!(value, Value::Null | Value::Bool(false)))
        {
            return None;
        }
        self.video_url
            .as_deref()
            .filter(|url| is_direct_audio_url(url) && !self.is_encoded_media_url(url))
    }

    fn is_encoded_media_url(&self, url: &str) -> bool {
        self.extra
            .get("media_urls")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().filter_map(Value::as_object).any(|media| {
                    media.get("url").and_then(Value::as_str) == Some(url)
                        && !is_unencoded_media(media)
                })
            })
    }

    fn progressive_m4a_url(&self) -> Option<&str> {
        self.extra
            .get("media_urls")?
            .as_array()?
            .iter()
            .filter_map(Value::as_object)
            .find_map(|media| {
                let delivery = media.get("delivery")?.as_str()?;
                let content_type = media.get("content_type")?.as_str()?;
                let url = media.get("url")?.as_str()?;
                (delivery.eq_ignore_ascii_case("progressive")
                    && is_m4a_content_type(content_type)
                    && is_unencoded_media(media)
                    && !self.is_encoded_media_url(url)
                    && is_usable_audio_url(url))
                .then_some(url)
            })
    }
}

fn is_unencoded_media(media: &serde_json::Map<String, Value>) -> bool {
    let unencoded = match media.get("encoding") {
        None | Some(Value::Null) => true,
        Some(Value::String(encoding)) => encoding.is_empty(),
        _ => false,
    };
    unencoded
        && matches!(
            media.get("encrypted"),
            None | Some(Value::Null) | Some(Value::Bool(false))
        )
}

fn is_direct_audio_url(url: &str) -> bool {
    is_usable_audio_url(url)
        && reqwest::Url::parse(url).ok().is_some_and(|parsed| {
            parsed.scheme() == "https"
                && parsed.host_str().is_some()
                && parsed.username().is_empty()
                && parsed.password().is_none()
        })
}

fn media_extension(content_type: &str) -> Option<&'static str> {
    match content_type.trim().to_ascii_lowercase().as_str() {
        "mp3" | "audio/mpeg" => Some("mp3"),
        "m4a" | "m4a-opus" | "audio/mp4" => Some("m4a"),
        _ => None,
    }
}

fn is_usable_audio_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return false;
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    !path.ends_with("/api/forbidden")
}

fn is_m4a_content_type(content_type: &str) -> bool {
    media_extension(content_type) == Some("m4a")
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ClipMetadata {
    pub tags: Option<String>,
    pub negative_tags: Option<String>,
    pub prompt: Option<String>,
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infill: Option<bool>,
    pub avg_bpm: Option<f64>,
    #[serde(default)]
    pub has_stem: bool,
    #[serde(default)]
    pub is_remix: bool,
    #[serde(default)]
    pub make_instrumental: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_mumble: Option<bool>,
    #[serde(rename = "type")]
    pub clip_type: Option<String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ClipActionConfig {
    #[serde(default)]
    pub actions: Vec<ClipAction>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ClipActionConfig {
    pub fn action(&self, action_type: &str) -> Option<&ClipAction> {
        self.actions
            .iter()
            .find(|action| action.action_type.as_deref() == Some(action_type))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClipAction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::{Clip, Value};

    #[test]
    fn current_clip_fields_round_trip_without_an_extra_wrapper() {
        let clip: Clip = serde_json::from_value(serde_json::json!({
            "id": "clip-1",
            "title": "Demo",
            "status": "complete",
            "model_name": "chirp-carp",
            "created_at": "2026-07-19T00:00:00Z",
            "is_trashed": false,
            "is_download_unlocked": true,
            "allow_comments": true,
            "action_config": {
                "surface": "song_actions",
                "actions": [{
                    "action_type": "download_song",
                    "disabled": false,
                    "visible": true,
                    "entitlement_reason": "subscribed"
                }]
            },
            "ownership": {"ownership_reason": "subscribed"},
            "media_urls": [{
                "url": "https://cdn.example/demo.mp3",
                "content_type": "mp3",
                "delivery": "progressive"
            }],
            "metadata": {
                "prompt": "[Verse]",
                "duration": 120.0,
                "infill": false,
                "model_badges": {"songcard": {"display_name": "v5.5"}},
                "priority": 10,
                "refund_credits": false,
                "uses_latest_model": true
            }
        }))
        .expect("deserialize current clip response");

        let action = clip
            .action_config
            .as_ref()
            .and_then(|config| config.action("download_song"))
            .expect("typed download action");
        assert_eq!(action.visible, Some(true));
        assert_eq!(action.disabled, Some(false));
        assert_eq!(clip.is_trashed, Some(false));
        assert_eq!(clip.is_download_unlocked, Some(true));
        assert_eq!(clip.metadata.infill, Some(false));

        let output = serde_json::to_value(clip).expect("serialize clip response");
        assert_eq!(output["allow_comments"], true);
        assert_eq!(output["is_download_unlocked"], true);
        assert_eq!(
            output["action_config"]["actions"][0]["action_type"],
            "download_song"
        );
        assert_eq!(output["action_config"]["surface"], "song_actions");
        assert_eq!(
            output["action_config"]["actions"][0]["entitlement_reason"],
            "subscribed"
        );
        assert_eq!(output["ownership"]["ownership_reason"], "subscribed");
        assert_eq!(
            output["media_urls"][0]["url"],
            "https://cdn.example/demo.mp3"
        );
        assert_eq!(
            output["metadata"]["model_badges"]["songcard"]["display_name"],
            "v5.5"
        );
        assert_eq!(output["metadata"]["priority"], 10);
        assert_eq!(output["metadata"]["refund_credits"], false);
        assert_eq!(output["metadata"]["uses_latest_model"], true);
        assert!(output.get("extra").is_none());
        assert!(output["metadata"].get("extra").is_none());
    }

    #[test]
    fn playback_url_prefers_a_usable_top_level_audio_url() {
        let clip: Clip = serde_json::from_value(serde_json::json!({
            "id": "clip-1",
            "title": "Song",
            "status": "complete",
            "model_name": "chirp-fenix",
            "audio_url": "https://cdn.example/clip-1.mp3",
            "created_at": "2026-08-30T00:00:00Z",
            "media_urls": [{
                "url": "https://stream.example/clip-1.m4a",
                "content_type": "m4a-opus",
                "delivery": "progressive"
            }]
        }))
        .expect("clip");

        assert_eq!(clip.playback_url(), Some("https://cdn.example/clip-1.mp3"));
    }

    #[test]
    fn playback_url_uses_progressive_m4a_for_forbidden_legacy_audio_url() {
        let clip: Clip = serde_json::from_value(serde_json::json!({
            "id": "clip-1",
            "title": "Song",
            "status": "complete",
            "model_name": "chirp-fenix",
            "audio_url": "https://studio-api.prod.suno.com/api/forbidden?reason=protected",
            "created_at": "2026-08-30T00:00:00Z",
            "media_urls": [
                {
                    "url": "https://cdn.example/clip-1.mp3",
                    "content_type": "mp3",
                    "delivery": "progressive"
                },
                {
                    "url": "https://stream.example/clip-1.m4a",
                    "content_type": "m4a-opus",
                    "delivery": "progressive"
                }
            ]
        }))
        .expect("clip");

        assert_eq!(
            clip.playback_url(),
            Some("https://stream.example/clip-1.m4a")
        );
    }

    #[test]
    fn playback_url_does_not_treat_protected_mp3_as_a_stream_fallback() {
        let clip: Clip = serde_json::from_value(serde_json::json!({
            "id": "clip-1",
            "title": "Song",
            "status": "complete",
            "model_name": "chirp-fenix",
            "audio_url": "https://studio-api.prod.suno.com/api/forbidden",
            "created_at": "2026-08-30T00:00:00Z",
            "media_urls": [{
                "url": "https://cdn.example/clip-1.mp3",
                "content_type": "mp3",
                "delivery": "progressive"
            }]
        }))
        .expect("clip");

        assert_eq!(clip.playback_url(), None);
    }
    fn direct_media_clip(audio_url: Option<&str>, media_urls: Value) -> Clip {
        serde_json::from_value(serde_json::json!({
            "id": "clip-1", "title": "Song", "status": "complete",
            "model_name": "chirp-hawk", "created_at": "2026-10-15T00:00:00Z",
            "audio_url": audio_url, "media_urls": media_urls
        }))
        .expect("clip")
    }

    #[test]
    fn encoded_progressive_m4a_is_never_native_playback_or_direct_download() {
        let clip = direct_media_clip(
            Some("https://studio-api.prod.suno.com/api/forbidden"),
            serde_json::json!([
                {"url":"https://cdn.example/clip-1.m4a", "content_type":"m4a-opus", "delivery":"progressive", "encoding":"1.0.0"}
            ]),
        );
        assert_eq!(clip.playback_url(), None);
        assert_eq!(clip.direct_audio_url("m4a"), None);
    }

    #[test]
    fn encoded_top_level_url_does_not_escape_media_encryption_metadata() {
        let clip = direct_media_clip(
            Some("https://cdn.example/clip-1.m4a"),
            serde_json::json!([
                {"url":"https://cdn.example/clip-1.m4a", "content_type":"m4a-opus", "delivery":"progressive", "encoding":"1.0.0"},
                {"url":"https://cdn.example/clip-1.m4a", "content_type":"m4a-opus", "delivery":"progressive"}
            ]),
        );
        assert_eq!(clip.playback_url(), None);
        assert_eq!(clip.direct_audio_url("m4a"), None);
    }

    #[test]
    fn direct_download_preserves_exact_signed_url_and_requires_matching_format() {
        let exact = "https://cdn.example/clip-1.mp3?signature=opaque&expires=123";
        let clip = direct_media_clip(Some(exact), serde_json::json!([]));
        assert_eq!(clip.direct_audio_url("mp3"), Some(exact));
        assert_eq!(clip.direct_audio_url("m4a"), None);
        assert_eq!(clip.direct_audio_url("wav"), None);
    }

    #[test]
    fn direct_download_uses_reported_unencoded_media_content_type() {
        let clip = direct_media_clip(
            None,
            serde_json::json!([
                {"url":"https://cdn.example/encoded", "content_type":"m4a-opus", "delivery":"progressive", "encoding":"1.0.0"},
                {"url":"https://cdn.example/plain?signature=opaque", "content_type":"m4a-opus", "delivery":"progressive", "encoding":null}
            ]),
        );
        assert_eq!(
            clip.direct_audio_url("m4a"),
            Some("https://cdn.example/plain?signature=opaque")
        );
    }

    #[test]
    fn direct_download_rejects_malformed_encryption_and_transport_metadata() {
        for encoding in [
            serde_json::json!(false),
            serde_json::json!(0),
            serde_json::json!({}),
            serde_json::json!("unknown"),
        ] {
            let clip = direct_media_clip(
                None,
                serde_json::json!([
                    {"url":"https://cdn.example/clip-1.m4a", "content_type":"m4a-opus", "delivery":"progressive", "encoding":encoding}
                ]),
            );
            assert_eq!(clip.direct_audio_url("m4a"), None);
            assert_eq!(clip.playback_url(), None);
        }
        for url in [
            "http://cdn.example/clip-1.mp3",
            "https://user:pass@cdn.example/clip-1.mp3",
            "file:///tmp/song.mp3",
            "https://cdn.example/api/forbidden?fake=.mp3",
        ] {
            let clip = direct_media_clip(Some(url), serde_json::json!([]));
            assert_eq!(clip.direct_audio_url("mp3"), None);
        }
        for media in [
            serde_json::json!({"url":"https://cdn.example/clip-1.m4a", "content_type":"m4a-unknown", "delivery":"progressive"}),
            serde_json::json!({"url":"https://cdn.example/clip-1.m4a", "content_type":"m4a-opus", "delivery":"streaming"}),
            serde_json::json!({"url":"https://cdn.example/clip-1.m4a", "content_type":"m4a-opus", "delivery":"progressive", "encrypted":true}),
        ] {
            let clip = direct_media_clip(None, serde_json::json!([media]));
            assert_eq!(clip.direct_audio_url("m4a"), None);
            assert_eq!(clip.playback_url(), None);
        }
    }
    #[test]
    fn direct_video_rejects_stale_encoded_or_invalid_urls() {
        let mut clip = direct_media_clip(None, serde_json::json!([]));
        clip.video_url = Some("https://cdn.example/video.mp4".into());
        assert_eq!(
            clip.direct_video_url(),
            Some("https://cdn.example/video.mp4")
        );
        for stale in [serde_json::json!(true), serde_json::json!("unknown")] {
            clip.metadata.extra.insert("video_is_stale".into(), stale);
            assert_eq!(clip.direct_video_url(), None);
        }
        clip.metadata.extra.clear();
        clip.extra.insert(
            "media_urls".into(),
            serde_json::json!([{"url":"https://cdn.example/video.mp4","encoding":"1.0.0"}]),
        );
        assert_eq!(clip.direct_video_url(), None);
        clip.extra.clear();
        for url in [
            "",
            "http://cdn.example/video.mp4",
            "file:///video.mp4",
            "https://u:p@cdn.example/video.mp4",
            "https://cdn.example/api/forbidden",
        ] {
            clip.video_url = Some(url.into());
            assert_eq!(clip.direct_video_url(), None);
        }
    }
}
