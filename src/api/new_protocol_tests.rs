use super::*;
use serde_json::{Value, json};
fn new_billing() -> String {
    let mut info: Value = serde_json::from_str(&billing_with_features(&["custom_models"])).unwrap();
    info["models"] = json!([{"name":"v6","external_key":"chirp-hawk","can_use":true,"is_default_model":true,"description":"fixture","capabilities":["all"],"features":["create_control_sliders"],"max_lengths":{"prompt":5000,"tags":1000,"gpt_description_prompt":3000},"major_version":6},
    {"name":"v6-mini","external_key":"chirp-goose","can_use":true,"is_default_model":false,"description":"fixture","capabilities":["all"],"features":["create_control_sliders"],"max_lengths":{"prompt":5000,"tags":1000,"gpt_description_prompt":3000},"major_version":6}]);
    info["agentic_limits"] = json!({"max_atoms":2,"max_images":1,"max_audio_refs":1});
    info.to_string()
}
fn album_fixture(id: &str, public: bool) -> String {
    json!({"metadata":{"id":id,"name":"Album","is_public":public},"relationship":{"is_owned":true,"can_edit":true,"is_trashed":false},"bio":{"description":""},"album":{"album_type":"lp","album_status":if public{"published"}else{"draft"}}}).to_string()
}
#[tokio::test]
async fn new_custom_bases_and_shared_reads_use_exact_routes() {
    let server = MockServer::response_sequence(vec![
        (
            200,
            json!({"options":[{"base_model":"chirp-goose","name":"v6-mini","is_default":true}]})
                .to_string(),
        ),
        (
            200,
            json!({"items":[],"generation_config":{"major_version":6},"next_cursor":"next"})
                .to_string(),
        ),
    ])
    .await;
    let client = server.client();
    assert_eq!(
        client.custom_model_bases().await.unwrap().options[0].base_model,
        "chirp-goose"
    );
    assert_eq!(
        client
            .shared_custom_models(Some("first"))
            .await
            .unwrap()
            .next_cursor
            .as_deref(),
        Some("next")
    );
    let requests = server.captured_all().await;
    assert_eq!(requests[0].path, "/api/custom-model/bases/");
    assert_eq!(
        requests[1].path,
        "/api/custom-model/shared?limit=100&cursor=first"
    );
    assert!(requests.iter().all(|r| r.method == "GET"));
}
#[tokio::test]
async fn new_training_base_is_resolved_before_any_source_reads() {
    let server = MockServer::response_sequence(vec![
        (200, new_billing()),
        (200, json!({"flags":{"custom-model-ui":true}}).to_string()),
        (
            200,
            json!({"options":[{"base_model":"chirp-hawk","name":"v6","is_default":true}]})
                .to_string(),
        ),
    ])
    .await;
    let error = server
        .client()
        .create_custom_model(
            &(1..=6).map(|i| format!("clip-{i}")).collect::<Vec<_>>(),
            "Sound",
            false,
            Some("chirp-goose"),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("base model"));
    let requests = server.captured_all().await;
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|r| r.method == "GET"));
}
#[tokio::test]
async fn new_speech_script_preserves_speech_metadata_without_music_duration() {
    let server=MockServer::response_sequence(vec![(200,new_billing()),(200,json!({"flags":{"aug-creativity":true}}).to_string()),(200,r#"{"required":false}"#.into()),(200,json!({"clips":[serde_json::from_str::<Value>(&complete_clip_fixture("speech-1",None,None)).unwrap()]}).to_string())]).await;
    let mut req = GenerateRequest::new("chirp-goose", "custom");
    req.prompt = "Good evening".into();
    req.tags = Some("quiet voice".into());
    req.metadata.is_speech = Some(true);
    req.metadata.backing_music = Some(false);
    req.metadata.control_sliders = Some(ControlSliders {
        aug_creativity: Some(1.0),
        weirdness_constraint: None,
        style_weight: None,
        audio_weight: None,
    });
    server.client().generate(&req).await.unwrap();
    let requests = server.captured_all().await;
    let body: Value = serde_json::from_str(&requests[3].body).unwrap();
    assert_eq!(requests[3].path, "/api/generate/v2-web/");
    assert_eq!(body["metadata"]["is_speech"], true);
    assert_eq!(body["metadata"]["backing_music"], false);
    assert!(body.get("duration").is_none());
    assert_eq!(body["mv"], "chirp-goose");
}
#[tokio::test]
async fn new_speech_default_variety_follows_gate_but_explicit_variety_requires_it() {
    for enabled in [false, true] {
        let server = MockServer::response_sequence(vec![
            (200, new_billing()),
            (200, json!({"flags":{"aug-creativity":enabled}}).to_string()),
        ])
        .await;
        let mut req = GenerateRequest::new("chirp-goose", "custom");
        req.prompt = "Good evening".into();
        req.metadata.is_speech = Some(true);
        server
            .client()
            .prepare_generation_request(&mut req)
            .await
            .unwrap();
        assert_eq!(
            req.metadata
                .control_sliders
                .as_ref()
                .and_then(|s| s.aug_creativity),
            enabled.then_some(1.0)
        );
        assert!(req.duration.is_none());
        assert!(
            server
                .captured_all()
                .await
                .iter()
                .all(|r| r.method == "GET")
        );
    }
    let server = MockServer::response_sequence(vec![
        (200, new_billing()),
        (200, json!({"flags":{"aug-creativity":false}}).to_string()),
    ])
    .await;
    let mut req = GenerateRequest::new("chirp-goose", "custom");
    req.prompt = "Good evening".into();
    req.metadata.is_speech = Some(true);
    req.metadata.control_sliders = Some(ControlSliders {
        aug_creativity: Some(2.0),
        weirdness_constraint: None,
        style_weight: None,
        audio_weight: None,
    });
    assert!(
        server
            .client()
            .prepare_generation_request(&mut req)
            .await
            .unwrap_err()
            .to_string()
            .contains("aug-creativity")
    );
    assert!(
        server
            .captured_all()
            .await
            .iter()
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn new_sounds_auto_selects_capable_noncustom_model_without_version_guessing() {
    let mut billing: Value = serde_json::from_str(&new_billing()).unwrap();
    let mut custom = billing["models"][0].clone();
    custom["external_key"] = json!("chirp-custom:own");
    custom["badges"] = json!(["custom"]);
    let mut music = billing["models"][0].clone();
    music["is_default_model"] = json!(false);
    music["capabilities"] = json!(["generate"]);
    let mut sound = billing["models"][1].clone();
    sound["external_key"] = json!("advertised-sound-model");
    sound["major_version"] = json!(5);
    sound["capabilities"] = json!(["sound"]);
    billing["models"] = json!([custom, music, sound]);
    let server = MockServer::response_sequence(vec![
        (200, billing.to_string()),
        (200, json!({"flags":{"create-sounds":true}}).to_string()),
    ])
    .await;
    let mut req = GenerateRequest::new("auto", "custom");
    req.task = Some("sound".into());
    server
        .client()
        .prepare_generation_request(&mut req)
        .await
        .unwrap();
    assert_eq!(req.mv, "advertised-sound-model");
    assert_eq!(server.captured_all().await.len(), 2);

    let server = MockServer::json(&billing.to_string()).await;
    let mut req = GenerateRequest::new("chirp-custom:own", "custom");
    req.task = Some("sound".into());
    assert!(
        server
            .client()
            .prepare_generation_request(&mut req)
            .await
            .unwrap_err()
            .to_string()
            .contains("Sounds")
    );
    assert_eq!(server.captured_all().await.len(), 1);
}
#[tokio::test]
async fn new_simple_auto_skips_incompatible_defaults_and_selects_usable_v6() {
    let mut billing: Value = serde_json::from_str(&new_billing()).unwrap();
    let supported = billing["models"][0].clone();
    let mut old_default = supported.clone();
    old_default["external_key"] = json!("chirp-fenix");
    old_default["major_version"] = json!(5);
    let mut no_generate = supported.clone();
    no_generate["external_key"] = json!("v6-sound-only");
    no_generate["capabilities"] = json!(["sound"]);
    let mut disabled = supported.clone();
    disabled["external_key"] = json!("v6-unavailable");
    disabled["can_use"] = json!(false);
    let mut eligible = supported;
    eligible["is_default_model"] = json!(false);
    for incompatible in [&old_default, &no_generate, &disabled] {
        billing["models"] = json!([incompatible, eligible]);
        let server = MockServer::response_sequence(vec![
            (200, billing.to_string()),
            (200, json!({"flags":{"agentic-simple":true}}).to_string()),
        ])
        .await;
        let mut req = GenerateRequest::new("auto", "simple");
        req.task = Some("agentic_thinking".into());
        server
            .client()
            .prepare_generation_request(&mut req)
            .await
            .unwrap();
        assert_eq!(req.mv, "chirp-hawk");
        assert!(
            server
                .captured_all()
                .await
                .iter()
                .all(|r| r.method == "GET")
        );
    }
    // An explicit incompatible selector must fail rather than silently switch models.
    billing["models"] = json!([old_default, eligible]);
    let server = MockServer::json(&billing.to_string()).await;
    let mut req = GenerateRequest::new("chirp-fenix", "simple");
    req.task = Some("agentic_thinking".into());
    assert!(
        server
            .client()
            .prepare_generation_request(&mut req)
            .await
            .unwrap_err()
            .to_string()
            .contains("Agentic Simple")
    );
    assert_eq!(req.mv, "chirp-fenix");
    assert_eq!(server.captured_all().await.len(), 1);
    // No compatible model must stop before reading gates or submitting.
    billing["models"] = json!([no_generate, disabled]);
    let server = MockServer::json(&billing.to_string()).await;
    let mut req = GenerateRequest::new("auto", "simple");
    req.task = Some("agentic_thinking".into());
    assert!(
        server
            .client()
            .prepare_generation_request(&mut req)
            .await
            .is_err()
    );
    assert_eq!(server.captured_all().await.len(), 1);
}

#[tokio::test]
async fn new_simple_and_sounds_reject_disabled_session_before_submission() {
    for task in ["agentic_thinking", "sound"] {
        let server = MockServer::response_sequence(vec![
            (200, new_billing()),
            (200, json!({"flags":{}}).to_string()),
        ])
        .await;
        let mut req = GenerateRequest::new("chirp-hawk", "simple");
        req.task = Some(task.into());
        assert!(
            server
                .client()
                .generate(&req)
                .await
                .unwrap_err()
                .to_string()
                .contains("current session")
        );
        assert_eq!(server.captured_all().await.len(), 2);
    }
}
#[tokio::test]
async fn new_simple_rejects_account_attachment_limits_before_challenge_or_write() {
    let server = MockServer::response_sequence(vec![
        (200, new_billing()),
        (200, json!({"flags":{"agentic-simple":true}}).to_string()),
    ])
    .await;
    let mut req = GenerateRequest::new("chirp-hawk", "simple");
    req.task = Some("agentic_thinking".into());
    req.user_uploaded_image_ids = vec!["one".into(), "two".into()];
    assert!(
        server
            .client()
            .generate(&req)
            .await
            .unwrap_err()
            .to_string()
            .contains("limits")
    );
    assert_eq!(server.captured_all().await.len(), 2);
}
#[tokio::test]
async fn new_album_create_is_draft_with_source_and_requires_exact_readback() {
    let server = MockServer::response_sequence(vec![
        (200, json!({"flags":{"can-create-album":true}}).to_string()),
        (200, json!({"metadata":{"id":"source"},"relationship":{"is_owned":true,"can_edit":true,"is_trashed":false}}).to_string()),
        (200, json!({"id":"new-album"}).to_string()),
        (200, album_fixture("new-album", false)),
    ])
    .await;
    server
        .client()
        .create_album("Album", "", "lp", Some("source"))
        .await
        .unwrap();
    let requests = server.captured_all().await;
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2].method, "POST");
    assert_eq!(requests[2].path, "/api/playlist/create/");
    assert_eq!(
        serde_json::from_str::<Value>(&requests[2].body).unwrap(),
        json!({"name":"Album","description":"","creation_source":"playlist_make_album","playlist_type":"album","album_type":"lp","album_status":"draft","source_playlist_id":"source"})
    );
    assert_eq!(requests[3].path, "/api/playlist/v2/new-album");
}
#[tokio::test]
async fn new_album_update_rejects_nonowner_and_ordinary_playlist() {
    for raw in [
        json!({"metadata":{"id":"album"},"relationship":{"is_owned":false,"can_edit":true,"is_trashed":false},"album":{}}),
        json!({"metadata":{"id":"album"},"relationship":{"is_owned":true,"can_edit":true,"is_trashed":false}}),
    ] {
        let server = MockServer::json(&raw.to_string()).await;
        assert!(
            server
                .client()
                .set_album("album", Some("New"), None, None)
                .await
                .is_err()
        );
        assert_eq!(server.captured_all().await.len(), 1);
    }
}
#[tokio::test]
async fn new_album_update_preserves_unmentioned_fields_and_reads_back() {
    let updated =
        json!({"metadata":{"id":"album","name":"New"},"album":{"album_type":"ep"}}).to_string();
    let server = MockServer::response_sequence(vec![
        (200, album_fixture("album", false)),
        (200, "{}".into()),
        (200, updated),
    ])
    .await;
    server
        .client()
        .set_album("album", Some("New"), None, Some("ep"))
        .await
        .unwrap();
    let requests = server.captured_all().await;
    assert_eq!(requests[1].method, "PATCH");
    assert_eq!(
        serde_json::from_str::<Value>(&requests[1].body).unwrap(),
        json!({"metadata":{"name":"New"},"album":{"album_type":"ep"}})
    );
}

#[tokio::test]
async fn new_album_add_requires_known_account_and_explicit_track_ownership() {
    let client =
        SunoClient::new_for_tests("http://127.0.0.1:1".into(), AuthState::default()).unwrap();
    assert!(
        client
            .add_album_clips("album", &["clip-1".into()])
            .await
            .unwrap_err()
            .to_string()
            .contains("identity")
    );
    let clip = complete_clip_fixture("clip-1", None, None);
    let mut clip: Value = serde_json::from_str(&clip).unwrap();
    clip["is_trashed"] = json!(false);
    clip.as_object_mut().unwrap().remove("user_id");
    let server = MockServer::response_sequence(vec![
        (200, album_fixture("album", false)),
        (200, clip.to_string()),
    ])
    .await;
    assert!(
        server
            .client()
            .add_album_clips("album", &["clip-1".into()])
            .await
            .unwrap_err()
            .to_string()
            .contains("owned")
    );
    assert!(
        server
            .captured_all()
            .await
            .iter()
            .all(|r| r.method == "GET")
    );
}
#[tokio::test]
async fn new_album_publish_verifies_album_and_every_reported_track() {
    let mut clip: Value =
        serde_json::from_str(&complete_clip_fixture("clip-1", None, None)).unwrap();
    clip["is_public"] = json!(true);
    let server = MockServer::response_sequence(vec![
        (200, json!({"flags":{"can-publish-album":true}}).to_string()),
        (200, album_fixture("album", false)),
        (
            200,
            json!({"success":true,"published_clip_ids":["clip-1"]}).to_string(),
        ),
        (200, album_fixture("album", true)),
        (200, clip.to_string()),
    ])
    .await;
    let result = server.client().publish_album("album", true).await.unwrap();
    assert_eq!(result["result"]["published_clip_ids"], json!(["clip-1"]));
    let requests = server.captured_all().await;
    assert_eq!(requests[2].path, "/api/playlist/v2/album/album/publish");
    assert_eq!(requests[2].method, "POST");
    assert_eq!(
        serde_json::from_str::<Value>(&requests[2].body).unwrap(),
        json!({"use_album_cover":true})
    );
}
#[tokio::test]
async fn new_album_publish_incomplete_response_is_ambiguous_and_never_replayed() {
    let server = MockServer::response_sequence(vec![
        (200, json!({"flags":{"can-publish-album":true}}).to_string()),
        (200, album_fixture("album", false)),
        (200, json!({"success":true}).to_string()),
    ])
    .await;
    assert_eq!(
        server
            .client()
            .publish_album("album", false)
            .await
            .unwrap_err()
            .error_code(),
        "ambiguous_mutation"
    );
    assert_eq!(server.captured_all().await.len(), 3);
}
#[tokio::test]
async fn new_album_publish_server_error_is_sent_once() {
    let server = MockServer::response_sequence(vec![
        (200, json!({"flags":{"can-publish-album":true}}).to_string()),
        (200, album_fixture("album", false)),
        (503, "failure".into()),
    ])
    .await;
    assert_eq!(
        server
            .client()
            .publish_album("album", false)
            .await
            .unwrap_err()
            .error_code(),
        "ambiguous_mutation"
    );
    assert_eq!(server.captured_all().await.len(), 3);
}
#[tokio::test]
async fn new_album_list_is_native_read_feed_with_cursor() {
    let server = MockServer::json(r#"{"feed":{"items":[],"next_cursor":"next"}}"#).await;
    server.client().list_albums(Some("first")).await.unwrap();
    let request = server.captured().await;
    assert_eq!(request.path, "/api/unified/feed");
    assert_eq!(
        serde_json::from_str::<Value>(&request.body).unwrap(),
        json!({"feed_id":"library_albums","page_size":20,"cursor":"first"})
    );
}
#[tokio::test]
async fn new_zip_prepare_keeps_exact_format_and_rejects_bad_response_without_replay() {
    for response in [
        json!({"download_url":"https://cdn.example.com/result.zip"}),
        json!({"download_url":"javascript:bad"}),
        json!({"download_url":"https://cdn.example.com/result.zip","failed_clips":["clip-1"]}),
    ] {
        let server = MockServer::json(&response.to_string()).await;
        let result = server
            .client()
            .prepare_download_zip(
                &["clip-1".into()],
                "Workspace",
                super::super::download::DownloadZipFormat::M4a,
            )
            .await;
        if response["download_url"] == "javascript:bad" {
            assert_eq!(result.unwrap_err().error_code(), "ambiguous_mutation");
        } else {
            let result = result.unwrap();
            assert_eq!(result["failed_clips"], response["failed_clips"]);
        }
        let requests = server.captured_all().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/api/download/clips/zip/prepare");
        let mut body = serde_json::from_str::<Value>(&requests[0].body).unwrap();
        let batch = body.as_object_mut().unwrap().remove("batch_id").unwrap();
        assert!(uuid::Uuid::parse_str(batch.as_str().unwrap()).is_ok());
        assert_eq!(
            body,
            json!({"clip_ids":["clip-1"],"workspace_name":"Workspace","format":"m4a"})
        );
    }
}

#[tokio::test]
async fn new_custom_model_rename_requires_owned_model_and_confirms_the_new_name() {
    let mut before: Value = serde_json::from_str(&new_billing()).unwrap();
    before["models"] = json!([{"id":"custom-id","name":"Old","external_key":"chirp-custom-1","can_use":true,"is_default_model":false,"description":"fixture","badges":["custom"],"max_lengths":{}}]);
    let mut after = before.clone();
    after["models"][0]["name"] = json!("New");
    let server = MockServer::response_sequence(vec![
        (200, before.to_string()),
        (200, "{}".into()),
        (200, after.to_string()),
    ])
    .await;
    server
        .client()
        .rename_custom_model("custom-id", "New")
        .await
        .unwrap();
    let requests = server.captured_all().await;
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].path, "/api/custom-model/edit/");
    assert_eq!(
        serde_json::from_str::<Value>(&requests[1].body).unwrap(),
        json!({"id":"custom-id","name":"New"})
    );
    let server = MockServer::json(&new_billing()).await;
    assert!(
        server
            .client()
            .rename_custom_model("custom-id", "New")
            .await
            .is_err()
    );
    assert_eq!(server.captured_all().await.len(), 1);
}

#[tokio::test]
async fn new_custom_model_rename_uses_fifty_character_limit_separate_from_training() {
    let name = "N".repeat(50);
    let mut before: Value = serde_json::from_str(&new_billing()).unwrap();
    before["models"] = json!([{"id":"custom-id","name":"Old","external_key":"chirp-custom-1","can_use":true,"is_default_model":false,"description":"fixture","badges":["custom"],"max_lengths":{}}]);
    let mut after = before.clone();
    after["models"][0]["name"] = json!(name);
    let server = MockServer::response_sequence(vec![
        (200, before.to_string()),
        (200, "{}".into()),
        (200, after.to_string()),
    ])
    .await;
    server
        .client()
        .rename_custom_model("custom-id", &name)
        .await
        .unwrap();
    let requests = server.captured_all().await;
    assert_eq!(requests.len(), 3);
    assert_eq!(
        serde_json::from_str::<Value>(&requests[1].body).unwrap()["name"],
        name
    );

    let client =
        SunoClient::new_for_tests("http://127.0.0.1:1".into(), AuthState::default()).unwrap();
    assert!(
        client
            .rename_custom_model("custom-id", &"N".repeat(51))
            .await
            .unwrap_err()
            .to_string()
            .contains("1..50")
    );
}
#[tokio::test]
async fn new_training_default_is_not_guessed_when_bases_are_ambiguous() {
    let server=MockServer::response_sequence(vec![(200,new_billing()),(200,json!({"flags":{"custom-model-ui":true}}).to_string()),(200,json!({"options":[{"base_model":"chirp-hawk","name":"v6","is_default":false},{"base_model":"chirp-goose","name":"v6-mini","is_default":false}]}).to_string())]).await;
    let error = server
        .client()
        .create_custom_model(
            &(1..=6).map(|i| format!("clip-{i}")).collect::<Vec<_>>(),
            "Sound",
            true,
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ambiguous"));
    assert_eq!(server.captured_all().await.len(), 3);
}

#[tokio::test]
async fn new_zip_missing_archive_preserves_failed_clips_and_batch_identity() {
    let server =
        MockServer::json(r#"{"failed_clips":[{"clip_id":"clip-1","reason":"unavailable"}]}"#).await;
    let error = server
        .client()
        .prepare_download_zip(
            &["clip-1".into()],
            "Workspace",
            super::super::download::DownloadZipFormat::Mp3,
        )
        .await
        .unwrap_err();
    assert_eq!(error.error_code(), "partial_mutation");
    let details = error.details().unwrap();
    assert_eq!(details["failed_clips"][0]["clip_id"], "clip-1");
    assert!(uuid::Uuid::parse_str(details["batch_id"].as_str().unwrap()).is_ok());
    assert_eq!(server.captured_all().await.len(), 1);
}

#[tokio::test]
async fn new_zip_prepare_never_replays_uncertain_http_outcomes() {
    for status in [307, 500] {
        let server = MockServer::response_sequence(vec![(status, "{}".into())]).await;
        let error = server
            .client()
            .prepare_download_zip(
                &["clip-1".into()],
                "Workspace",
                super::super::download::DownloadZipFormat::M4a,
            )
            .await
            .unwrap_err();
        assert_eq!(error.error_code(), "ambiguous_mutation");
        assert_eq!(server.captured_all().await.len(), 1);
    }
}
