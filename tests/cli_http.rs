use assert_cmd::Command;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    body: String,
}

fn isolated_test_home(prefix: &str) -> PathBuf {
    let test_home = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&test_home).expect("test home");
    test_home
}

fn write_auth(test_home: &Path) {
    let config_dir = test_home.join(".config").join("sunox");
    std::fs::create_dir_all(&config_dir).expect("auth directory");
    std::fs::write(
        config_dir.join("auth.json"),
        serde_json::to_vec(&serde_json::json!({
            "jwt": "e30.eyJzdWIiOiJ1c2VyLXRlc3QiLCJleHAiOjQxMDI0NDQ4MDB9.c2ln",
            "browser_environment": {
                "browser_source": "cli-http-test",
                "user_agent": "Mozilla/5.0 Chrome/136.0.0.0",
                "accept_language": "en-US",
                "client_hints": {
                    "sec_ch_ua": "\"Chromium\";v=\"136\"",
                    "sec_ch_ua_mobile": "?0",
                    "sec_ch_ua_platform": "\"macOS\""
                }
            }
        }))
        .expect("auth json"),
    )
    .expect("write auth");
}

fn isolated_command(test_home: &Path, base_url: &str) -> Command {
    let mut command = Command::cargo_bin("sunox").expect("binary");
    command
        .env("HOME", test_home)
        .env("USERPROFILE", test_home)
        .env("APPDATA", test_home.join("AppData").join("Roaming"))
        .env("LOCALAPPDATA", test_home.join("AppData").join("Local"))
        .env("XDG_CONFIG_HOME", test_home.join(".config"))
        .env("XDG_DATA_HOME", test_home.join(".local").join("share"))
        .env("SUNOX_TEST_API_BASE_URL", base_url)
        .env_remove("SUNOX_DEFAULT_MODEL")
        .env_remove("SUNOX_POLL_INTERVAL_SECS")
        .env_remove("SUNOX_POLL_TIMEOUT_SECS")
        .env_remove("SUNOX_OUTPUT_DIR")
        .env_remove("SUNOX_SERIAL_MUTATIONS")
        .env_remove("SUNOX_CHALLENGE_BROWSER");
    command
}

fn serve_json_sequence(responses: Vec<String>) -> (String, Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let address = listener.local_addr().expect("mock address");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for response in responses {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "mock request deadline");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept mock request: {error}"),
                }
            };
            // Darwin can inherit O_NONBLOCK from the listener; receive deadlines need blocking IO.
            stream
                .set_nonblocking(false)
                .expect("blocking accepted connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("request timeout");
            let request = read_request(&mut stream);
            sender.send(request).expect("capture request");
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            );
            stream.write_all(wire.as_bytes()).expect("mock response");
        }
    });
    (format!("http://{address}"), receiver)
}

fn read_request(stream: &mut TcpStream) -> CapturedRequest {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut chunk).expect("read request");
        assert!(count > 0, "request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().expect("content length"))
            })
        })
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let count = stream.read(&mut chunk).expect("read request body");
        assert!(count > 0, "request closed before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    let request_line = headers.lines().next().expect("request line");
    let mut parts = request_line.split_whitespace();
    CapturedRequest {
        method: parts.next().expect("request method").to_string(),
        path: parts.next().expect("request path").to_string(),
        body: String::from_utf8(bytes[header_end..header_end + content_length].to_vec())
            .expect("request body utf8"),
    }
}

fn billing_fixture() -> String {
    serde_json::json!({
        "credits": 100,
        "total_credits_left": 100,
        "monthly_usage": 0,
        "monthly_limit": 2500,
        "is_active": true,
        "plan": {
            "id": "tier-pro",
            "name": "Pro Plan",
            "plan_key": "pro",
            "usage_plan_features": []
        },
        "models": [],
        "period": "month",
        "renews_on": null,
        "remaster_model_types": []
    })
    .to_string()
}

#[test]
fn clip_list_dispatches_through_the_public_cli_to_feed_v3() {
    let home = isolated_test_home("sunox-cli-http-read");
    write_auth(&home);
    let feed = serde_json::json!({
        "clips": [{
            "id": "clip-read-1",
            "title": "CLI contract",
            "status": "complete",
            "model_name": "chirp-fenix",
            "created_at": "2026-08-30T00:00:00Z"
        }],
        "next_cursor": null,
        "has_more": false
    })
    .to_string();
    let (base_url, requests) = serve_json_sequence(vec![feed]);

    let assertion = isolated_command(&home, &base_url)
        .args(["clip", "list", "--limit", "1", "--json"])
        .assert()
        .success();
    let output: serde_json::Value =
        serde_json::from_slice(&assertion.get_output().stdout).expect("CLI json");
    assert_eq!(output["data"]["clips"][0]["id"], "clip-read-1");

    let request = requests
        .recv_timeout(Duration::from_secs(1))
        .expect("feed request");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/api/feed/v3");
    let body: serde_json::Value = serde_json::from_str(&request.body).expect("feed request json");
    assert_eq!(body["limit"], 1);
    assert_eq!(body["filters"]["workspace"]["presence"], "True");
}

#[test]
fn clip_visibility_dispatches_auth_preflight_and_one_write_from_public_cli() {
    let home = isolated_test_home("sunox-cli-http-write");
    write_auth(&home);
    let (base_url, requests) = serve_json_sequence(vec![billing_fixture(), "{}".to_string()]);

    let assertion = isolated_command(&home, &base_url)
        .args(["clip", "publish", "clip-write-1", "--private", "--json"])
        .assert()
        .success();
    let output: serde_json::Value =
        serde_json::from_slice(&assertion.get_output().stdout).expect("CLI json");
    assert_eq!(
        output["data"]["clip_ids"],
        serde_json::json!(["clip-write-1"])
    );
    assert_eq!(output["data"]["is_public"], false);

    let preflight = requests
        .recv_timeout(Duration::from_secs(1))
        .expect("billing preflight");
    assert_eq!(preflight.method, "GET");
    assert_eq!(preflight.path, "/api/billing/info/");
    let mutation = requests
        .recv_timeout(Duration::from_secs(1))
        .expect("visibility write");
    assert_eq!(mutation.method, "POST");
    assert_eq!(mutation.path, "/api/gen/clip-write-1/set_visibility/");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&mutation.body).expect("mutation json"),
        serde_json::json!({"is_public": false, "submit_to_contest": false})
    );
    assert!(requests.recv_timeout(Duration::from_millis(100)).is_err());
}

#[test]
fn playlist_add_partial_failure_is_nonzero_and_preserves_recovery_journal() {
    let test_home = isolated_test_home("sunox-playlist-add-partial");
    write_auth(&test_home);
    let (base_url, requests) = serve_json_sequence(vec![billing_fixture(),
        serde_json::json!({"successes":["clip-a"],"failures":[{"id":"clip-b","code":"not_allowed"}]}).to_string()]);
    let assertion = isolated_command(&test_home, &base_url)
        .args([
            "--json",
            "playlist",
            "add",
            "playlist-a",
            "clip-a",
            "clip-b",
        ])
        .assert()
        .failure();
    let error: serde_json::Value = serde_json::from_slice(&assertion.get_output().stderr).unwrap();
    assert_eq!(error["error"]["code"], "partial_mutation");
    assert_eq!(
        error["error"]["details"]["succeeded_clip_ids"],
        serde_json::json!(["clip-a"])
    );
    assert_eq!(error["error"]["details"]["failed"][0]["clip_id"], "clip-b");
    let operations = test_home.join(".config/sunox/operations");
    assert_eq!(std::fs::read_dir(operations).unwrap().count(), 1);
    let captured: Vec<_> = requests.iter().collect();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[1].path, "/api/playlist/v2/playlist-a/tracks/add");
}

#[test]
fn reuse_mumble_checks_the_session_gate_before_any_generation() {
    let test_home = isolated_test_home("sunox-reuse-mumble-gate");
    write_auth(&test_home);
    let mut billing: serde_json::Value = serde_json::from_str(&billing_fixture()).unwrap();
    billing["models"] = serde_json::json!([{
        "name":"v6","external_key":"chirp-hawk","can_use":true,"is_default_model":true,
        "description":"fixture",
        "capabilities":["all"],"features":["reuse_styles_lyrics","mumble_mode","create_control_sliders"],
        "max_lengths":{},"badges":["pro"]
    }]);
    let source = serde_json::json!({"id":"mumble-source","title":"Mumble","status":"complete",
        "model_name":"chirp-hawk","created_at":"2026-09-30T00:00:00Z",
        "metadata":{"prompt":"","tags":"ambient","is_mumble":true,"make_instrumental":false}});
    let (base_url, requests) = serve_json_sequence(vec![
        billing.to_string(),
        source.to_string(),
        billing.to_string(),
        serde_json::json!({"flags":{"aug-creativity":true}}).to_string(),
    ]);
    let assertion = isolated_command(&test_home, &base_url)
        .args([
            "--json",
            "--quiet",
            "clip",
            "reuse",
            "mumble-source",
            "--no-captcha",
        ])
        .assert()
        .failure();
    let error: serde_json::Value = serde_json::from_slice(&assertion.get_output().stderr).unwrap();
    assert_eq!(error["error"]["code"], "config_error", "{error}");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("mumble-mode")
    );
    let captured: Vec<_> = requests.iter().collect();
    assert_eq!(captured.len(), 4);
    assert!(captured.iter().all(|r| r.method == "GET"));
}

#[test]
fn retired_opus_download_rejects_before_account_or_authorization_requests() {
    let test_home = isolated_test_home("sunox-retired-opus");
    write_auth(&test_home);
    let mut command = isolated_command(&test_home, "http://127.0.0.1:1");
    let output = command
        .args([
            "--json", "clip", "download", "clip-a", "--source", "official", "--format", "opus",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported_download_format"));
    std::fs::remove_dir_all(test_home).unwrap();
}

#[test]
fn external_hcaptcha_token_survives_turnstile_preflight_in_real_cli_submission() {
    let test_home = isolated_test_home("sunox-external-provider");
    write_auth(&test_home);
    let mut billing: serde_json::Value = serde_json::from_str(&billing_fixture()).unwrap();
    billing["models"] = serde_json::json!([{
        "name":"v6","external_key":"chirp-hawk","can_use":true,"is_default_model":true,
        "description":"fixture","capabilities":["all"],"features":[],
        "max_lengths":{},"badges":["pro"]
    }]);
    let generated = serde_json::json!({"clips":[{
        "id":"created-fixture","status":"submitted","title":"test",
        "model_name":"chirp-hawk","created_at":"2026-10-02T00:00:00Z"
    }]});
    let (base_url, requests) = serve_json_sequence(vec![
        billing.to_string(),
        billing.to_string(),
        serde_json::json!({"required":true,"captcha_version":2}).to_string(),
        generated.to_string(),
    ]);
    isolated_command(&test_home, &base_url)
        .args([
            "--json",
            "--quiet",
            "create",
            "short piano piece",
            "--token",
            "external-hcaptcha-fixture",
            "--token-provider",
            "hcaptcha",
        ])
        .assert()
        .success();
    let captured: Vec<_> = requests.iter().collect();
    assert_eq!(captured.len(), 4);
    assert_eq!(captured[2].path, "/api/c/check");
    assert_eq!(captured[3].method, "POST");
    assert_eq!(captured[3].path, "/api/generate/v2-web/");
    let body: serde_json::Value = serde_json::from_str(&captured[3].body).unwrap();
    assert_eq!(body["token_provider"], 1);
    assert_eq!(body["token"], "external-hcaptcha-fixture");
}

#[test]
fn ambiguous_external_token_never_reaches_generation_post() {
    let test_home = isolated_test_home("sunox-ambiguous-provider");
    write_auth(&test_home);
    let mut billing: serde_json::Value = serde_json::from_str(&billing_fixture()).unwrap();
    billing["models"] = serde_json::json!([{
        "name":"v6","external_key":"chirp-hawk","can_use":true,"is_default_model":true,
        "description":"fixture","capabilities":["all"],"features":[],"max_lengths":{},"badges":["pro"]
    }]);
    let (base_url, requests) = serve_json_sequence(vec![
        billing.to_string(),
        billing.to_string(),
        serde_json::json!({"required":true,"captcha_version":2}).to_string(),
    ]);
    let assertion = isolated_command(&test_home, &base_url)
        .args([
            "--json",
            "--quiet",
            "create",
            "short piano piece",
            "--token",
            "ambiguous-fixture",
        ])
        .assert()
        .failure();
    let error: serde_json::Value = serde_json::from_slice(&assertion.get_output().stderr).unwrap();
    assert_eq!(error["error"]["code"], "config_error");
    let captured: Vec<_> = requests.iter().collect();
    assert_eq!(captured.len(), 3);
    assert!(captured.iter().all(|r| r.path != "/api/generate/v2-web/"));
}

fn new_mode_billing() -> String {
    let mut billing: serde_json::Value = serde_json::from_str(&billing_fixture()).unwrap();
    billing["models"] = serde_json::json!([
        {"name":"v6","external_key":"chirp-hawk","can_use":true,"is_default_model":true,"description":"fixture","major_version":6,"capabilities":["all"],"features":["create_control_sliders"],"max_lengths":{"prompt":5000,"tags":1000,"gpt_description_prompt":3000}},
        {"name":"v6-mini","external_key":"chirp-goose","can_use":true,"is_default_model":false,"description":"fixture","major_version":6,"capabilities":["all"],"features":["create_control_sliders"],"max_lengths":{"prompt":5000,"tags":1000,"gpt_description_prompt":3000}}
    ]);
    billing["agentic_limits"] =
        serde_json::json!({"max_atoms":10,"max_images":5,"max_audio_refs":10});
    billing.to_string()
}

#[test]
fn current_creation_modes_dispatch_exact_public_request_shapes() {
    let session=serde_json::json!({"flags":{"aug-creativity":true,"agentic-simple":true,"create-sounds":true}}).to_string();
    let generated=serde_json::json!({"clips":[{"id":"generated","title":"Result","status":"complete","model_name":"chirp-goose","created_at":"2026-10-13T00:00:00Z"}]}).to_string();
    let cases: Vec<Vec<&str>> = vec![
        vec![
            "speech",
            "--script",
            "Good evening",
            "--tone",
            "Warm",
            "--no-music",
            "--vocal",
            "female",
        ],
        vec!["speech", "--describe", "A bedtime story"],
        vec![
            "sounds",
            "Warm bass",
            "--loop",
            "--bpm",
            "100",
            "--key",
            "C minor",
        ],
        vec![
            "simple",
            "Acoustic song",
            "--lyrics",
            "Hello",
            "--styles",
            "Soft",
        ],
    ];
    for arguments in cases {
        let home = isolated_test_home("sunox-new-mode-http");
        write_auth(&home);
        let mut responses = vec![new_mode_billing(), new_mode_billing(), session.clone()];
        if arguments[0] == "speech" && arguments[1] == "--describe" {
            responses.pop();
        }
        if arguments[0] == "simple" {
            responses.extend([new_mode_billing(), session.clone()]);
        }
        responses.extend([r#"{"required":false}"#.into(), generated.clone()]);
        let (base_url, requests) = serve_json_sequence(responses);
        let result = isolated_command(&home, &base_url)
            .args(&arguments)
            .args(["--quiet", "--no-captcha", "--json"])
            .assert()
            .success();
        let output: serde_json::Value =
            serde_json::from_slice(&result.get_output().stdout).unwrap();
        assert_eq!(output["data"]["clips"][0]["id"], "generated");
        let all: Vec<_> = requests.iter().collect();
        let submit = all.last().unwrap();
        assert_eq!(submit.method, "POST");
        assert_eq!(submit.path, "/api/generate/v2-web/");
        let body: serde_json::Value = serde_json::from_str(&submit.body).unwrap();
        assert!(body.get("duration").is_none());
        assert!(body["token"].is_null());
        assert!(body["token_provider"].is_null());
        match arguments[0] {
            "speech" if arguments[1] == "--script" => {
                assert_eq!(body["mv"], "chirp-goose");
                assert_eq!(body["metadata"]["is_speech"], true);
                assert_eq!(body["metadata"]["backing_music"], false);
                assert_eq!(body["metadata"]["vocal_gender"], "f");
                assert_eq!(body["prompt"], "Good evening");
                assert_eq!(body["tags"], "Warm");
                assert!(body.get("task").is_none());
            }
            "speech" => {
                assert_eq!(body["task"], "agentic_thinking");
                assert_eq!(body["gpt_description_prompt"], "A bedtime story");
                assert_eq!(body["metadata"]["is_speech"], true);
                assert!(body["metadata"].get("backing_music").is_none());
            }
            "sounds" => {
                assert_eq!(body["task"], "sound");
                assert_eq!(
                    body["metadata"]["sound_configs"],
                    serde_json::json!({"user_loop":true,"user_tempo":100,"user_key":"C minor"})
                );
                assert_eq!(body["make_instrumental"], true);
            }
            "simple" => {
                assert_eq!(body["task"], "agentic_thinking");
                assert_eq!(body["attached_lyrics"], "Hello");
                assert_eq!(body["attached_styles"], "Soft");
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn new_public_mutation_commands_obey_read_only_before_account_requests() {
    let home = isolated_test_home("sunox-new-read-only");
    for arguments in [
        vec!["speech", "--script", "Hello"],
        vec!["speech", "--describe", "A story"],
        vec!["simple", "A song"],
        vec!["sounds", "A bass"],
        vec!["album", "create", "--name", "Album"],
        vec!["album", "publish", "album-id"],
        vec!["models", "custom", "rename", "model-id", "--name", "New"],
        vec!["download-zip", "clip-1"],
    ] {
        isolated_command(&home, "http://127.0.0.1:1")
            .args(arguments)
            .args(["--read-only", "--json"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("--read-only blocks"));
    }
}

#[test]
fn new_album_publish_dispatch_uses_its_own_route_and_confirms_visibility() {
    let home = isolated_test_home("sunox-new-album-cli");
    write_auth(&home);
    let album = |public| {
        serde_json::json!({"metadata":{"id":"album-id","name":"Album","is_public":public},"relationship":{"is_owned":true,"can_edit":true,"is_trashed":false},"album":{"album_type":"lp"}}).to_string()
    };
    let (base_url, requests) = serve_json_sequence(vec![
        new_mode_billing(),
        serde_json::json!({"flags":{"can-publish-album":true}}).to_string(),
        album(false),
        serde_json::json!({"success":true,"published_clip_ids":[]}).to_string(),
        album(true),
    ]);
    isolated_command(&home, &base_url)
        .args(["album", "publish", "album-id", "--json"])
        .assert()
        .success();
    let all: Vec<_> = requests.iter().collect();
    assert_eq!(all.len(), 5);
    assert_eq!(all[3].path, "/api/playlist/v2/album-id/album/publish");
    assert_eq!(all[3].method, "POST");
    assert_eq!(all[4].method, "GET");
}

#[test]
fn simple_hydrates_exact_audio_reference_metadata_before_submit() {
    let home = isolated_test_home("sunox-simple-audio-reference");
    write_auth(&home);
    let session = serde_json::json!({"flags":{"agentic-simple":true}}).to_string();
    let clip=serde_json::json!({"id":"audio-ref","title":"Reference","status":"complete","model_name":"chirp-hawk","created_at":"2026-10-13T00:00:00Z","is_trashed":false,"metadata":{"duration":12.75,"tags":"piano","prompt":"Hello"}}).to_string();
    let generated=serde_json::json!({"clips":[{"id":"generated","title":"Result","status":"submitted","model_name":"chirp-hawk","created_at":"2026-10-13T00:00:00Z"}]}).to_string();
    let (base_url, requests) = serve_json_sequence(vec![
        new_mode_billing(),
        new_mode_billing(),
        session.clone(),
        clip,
        new_mode_billing(),
        session,
        r#"{"required":false}"#.into(),
        generated,
    ]);
    isolated_command(&home, &base_url)
        .args([
            "simple",
            "Make a song",
            "--audio-ref",
            "audio-ref",
            "--no-captcha",
            "--quiet",
            "--json",
        ])
        .assert()
        .success();
    let all: Vec<_> = requests.iter().collect();
    assert_eq!(all[3].path, "/api/clip/audio-ref");
    let body: serde_json::Value = serde_json::from_str(&all[7].body).unwrap();
    assert_eq!(
        body["audio_refs"],
        serde_json::json!([{"clip_id":"audio-ref","style_prompt":"piano","lyrics":"Hello","duration_s":12.75}])
    );
}

#[test]
fn cdn_download_rejects_encoded_media_without_export_or_authorization_requests() {
    for read_only in [false, true] {
        let home = isolated_test_home("sunox-cdn-encrypted");
        write_auth(&home);
        let clip = serde_json::json!({
            "id":"locked-clip", "title":"Protected playback", "status":"complete",
            "model_name":"chirp-hawk", "created_at":"2026-10-15T00:00:00Z",
            "is_download_unlocked":false, "audio_url":"https://studio-api.prod.suno.com/api/forbidden",
            "media_urls":[{"url":"https://media.example/locked.m4a", "delivery":"progressive",
                "content_type":"m4a-opus", "encoding":"1.0.0"}]
        }).to_string();
        let (base_url, requests) = serve_json_sequence(vec![clip]);
        let mut command = isolated_command(&home, &base_url);
        command.args(["download", "locked-clip", "--source", "cdn", "--json"]);
        if read_only {
            command.arg("--read-only");
        }
        let assertion = command.assert().failure();
        let error: serde_json::Value =
            serde_json::from_slice(&assertion.get_output().stderr).unwrap();
        assert_eq!(error["error"]["code"], "cdn_audio_unavailable");
        assert_eq!(error["error"]["details"]["authorization_post_sent"], false);
        let all: Vec<_> = requests.iter().collect();
        assert_eq!(all.len(), 1);
        assert_eq!(
            (all[0].method.as_str(), all[0].path.as_str()),
            ("GET", "/api/clip/locked-clip")
        );
    }
}

#[test]
fn cdn_download_rejects_format_conversion_before_account_reads() {
    let home = isolated_test_home("sunox-cdn-unsupported");
    for option in [
        vec!["--format", "wav"],
        vec!["--format", "opus"],
        vec!["--video"],
    ] {
        isolated_command(&home, "http://127.0.0.1:1")
            .args(["download", "clip-id", "--source", "cdn", "--json"])
            .args(option)
            .assert()
            .failure()
            .stderr(predicates::str::contains("supports only available MP3/M4A"));
    }
}

#[test]
fn zip_preparation_never_authorizes_sources_individually() {
    let home = isolated_test_home("sunox-zip-no-extra-authorization");
    write_auth(&home);
    let clip = serde_json::json!({
        "id":"locked-clip", "title":"Song", "status":"complete", "model_name":"chirp-hawk",
        "created_at":"2026-10-15T00:00:00Z", "is_trashed":false,"is_download_unlocked":false
    })
    .to_string();
    let (base_url, requests) = serve_json_sequence(vec![
        new_mode_billing(),
        r#"{"flags":{"multi-select-download-zip":true}}"#.into(),
        clip,
        // Stop at the returned URL so this test cannot transfer a real archive.
        r#"{"download_url":"javascript:invalid"}"#.into(),
    ]);
    let assertion = isolated_command(&home, &base_url)
        .args(["download-zip", "locked-clip", "--format", "m4a", "--output"])
        .arg(home.join("output"))
        .args(["--json", "--quiet"])
        .assert()
        .failure();
    let error: serde_json::Value = serde_json::from_slice(&assertion.get_output().stderr).unwrap();
    assert_eq!(error["error"]["code"], "ambiguous_mutation");
    let all: Vec<_> = requests.iter().collect();
    assert_eq!(all.len(), 4);
    assert_eq!(all[2].path, "/api/clip/locked-clip");
    assert_eq!(
        (all[3].method.as_str(), all[3].path.as_str()),
        ("POST", "/api/download/clips/zip/prepare")
    );
    let body: serde_json::Value = serde_json::from_str(&all[3].body).unwrap();
    assert!(uuid::Uuid::parse_str(body["batch_id"].as_str().unwrap()).is_ok());
    assert_eq!(body["clip_ids"], serde_json::json!(["locked-clip"]));
    assert!(
        all.iter()
            .all(|request| request.path != "/api/download/authorize")
    );
}

#[test]
fn zip_disabled_session_and_unavailable_formats_never_submit() {
    let home = isolated_test_home("sunox-zip-preflight");
    write_auth(&home);
    for format in ["wav", "opus"] {
        isolated_command(&home, "http://127.0.0.1:1")
            .args(["download-zip", "clip-id", "--format", format])
            .assert()
            .failure()
            .stderr(predicates::str::contains("invalid value"));
    }
    let (base_url, requests) = serve_json_sequence(vec![
        new_mode_billing(),
        r#"{"flags":{"multi-select-download-zip":false}}"#.into(),
    ]);
    isolated_command(&home, &base_url)
        .args(["download-zip", "clip-id", "--output"])
        .arg(home.join("output"))
        .args(["--json", "--quiet"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "does not enable multi-select-download-zip",
        ));
    assert_eq!(requests.iter().count(), 2);
}

#[test]
fn song_video_cli_submits_one_bodyless_post_and_returns_video_url_without_authorizing() {
    let home = isolated_test_home("sunox-song-video");
    write_auth(&home);
    let mut billing: serde_json::Value = serde_json::from_str(&new_mode_billing()).unwrap();
    billing["accessible_features"] = serde_json::json!([{"name":"generate_song_video"}]);
    let billing = billing.to_string();
    let source=serde_json::json!({"id":"video-clip","user_id":"user-test","status":"complete","is_trashed":false,"title":"Video","model_name":"chirp-hawk","created_at":"2026-10-15T00:00:00Z","video_url":"","is_download_unlocked":false}).to_string();
    let (base_url, requests) = serve_json_sequence(vec![
        billing.clone(),
        billing,
        source,
        "{}".into(),
        r#"{"status":"complete","video_url":"https://cdn.example/video.mp4"}"#.into(),
    ]);
    let assertion = isolated_command(&home, &base_url)
        .args([
            "clip",
            "generate-video",
            "video-clip",
            "--timeout",
            "5",
            "--json",
            "--quiet",
        ])
        .assert()
        .success();
    let result: serde_json::Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(result["data"]["video_url"], "https://cdn.example/video.mp4");
    let all: Vec<_> = requests.iter().collect();
    assert_eq!(all.len(), 5);
    assert_eq!(
        (all[3].method.as_str(), all[3].path.as_str()),
        ("POST", "/api/video/generate/video-clip/")
    );
    assert!(all[3].body.is_empty());
    assert!(all.iter().all(|r| r.path != "/api/download/authorize"));
}

#[cfg(unix)]
fn install_mock_video_tools(home: &Path, include_ffmpeg: bool) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("tools");
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["ffprobe", "ffmpeg"] {
        if name == "ffmpeg" && !include_ffmpeg {
            continue;
        }
        let path = bin.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    bin
}

#[cfg(unix)]
#[test]
fn default_video_source_read_only_blocks_render_without_authorization() {
    for arguments in [vec!["--read-only"], vec!["--generate-video", "--read-only"]] {
        let home = isolated_test_home("sunox-video-source-missing");
        write_auth(&home);
        let bin = install_mock_video_tools(&home, true);
        let clip=serde_json::json!({"id":"video-clip","user_id":"user-test","status":"complete","is_trashed":false,"title":"Video","model_name":"chirp-hawk","created_at":"","video_url":""}).to_string();
        let (base, requests) =
            serve_json_sequence(vec![clip, r#"{"status":"complete","video_url":""}"#.into()]);
        let mut command = isolated_command(&home, &base);
        command
            .env("PATH", &bin)
            .args(["download", "video-clip", "--json", "--quiet"])
            .args(&arguments);
        let assertion = command.assert().failure();
        let error: serde_json::Value =
            serde_json::from_slice(&assertion.get_output().stderr).unwrap();
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("--read-only blocks")
        );
        let all: Vec<_> = requests.iter().collect();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|request| request.method == "GET"));
    }
}

#[cfg(unix)]
#[test]
fn video_source_checks_dependencies_and_options_before_any_account_request() {
    let home = isolated_test_home("sunox-video-source-local-checks");
    let bin = install_mock_video_tools(&home, false);
    isolated_command(&home, "http://127.0.0.1:1")
        .env("PATH", &bin)
        .args([
            "download",
            "clip-id",
            "--source",
            "video",
            "--generate-video",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot start ffmpeg"));
    for args in [
        vec!["--source", "video", "--format", "wav"],
        vec!["--source", "video", "--format", "mp3"],
        vec!["--source", "official", "--generate-video"],
        vec!["--source", "official", "--video-timeout", "5"],
    ] {
        isolated_command(&home, "http://127.0.0.1:1")
            .args(["download", "clip-id", "--json"])
            .args(args)
            .assert()
            .failure()
            .stderr(predicates::str::contains("config_error"));
    }
}

#[cfg(unix)]
#[test]
fn default_download_generates_once_and_never_falls_back_to_allowance_on_bad_video() {
    let home = isolated_test_home("sunox-video-source-generated");
    write_auth(&home);
    let bin = install_mock_video_tools(&home, true);
    let mut billing: serde_json::Value = serde_json::from_str(&new_mode_billing()).unwrap();
    billing["accessible_features"] = serde_json::json!([{"name":"generate_song_video"}]);
    let billing = billing.to_string();
    let clip=serde_json::json!({"id":"video-clip","user_id":"user-test","status":"complete","is_trashed":false,"title":"Video","model_name":"chirp-hawk","created_at":"","video_url":""}).to_string();
    let (base, requests) = serve_json_sequence(vec![
        clip.clone(),
        clip.clone(),
        r#"{"status":"complete","video_url":""}"#.into(),
        billing.clone(),
        clip,
        billing,
        "{}".into(),
        r#"{"status":"complete","video_url":"https://cdn.example/api/forbidden"}"#.into(),
    ]);
    let assertion = isolated_command(&home, &base)
        .env("PATH", bin)
        .args(["download", "video-clip", "--video-timeout", "5", "--output"])
        .arg(home.join("output"))
        .args(["--quiet", "--json"])
        .assert()
        .failure();
    let error: serde_json::Value = serde_json::from_slice(&assertion.get_output().stderr).unwrap();
    assert_eq!(error["error"]["code"], "partial_mutation");
    assert_eq!(
        error["error"]["details"]["generated_video_clip_ids"],
        serde_json::json!(["video-clip"])
    );
    let all: Vec<_> = requests.iter().collect();
    assert_eq!(all.len(), 8);
    assert_eq!(
        all.iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
    assert_eq!(all[2].path, "/api/video/generate/video-clip/status/");
    assert_eq!(all[6].path, "/api/video/generate/video-clip/");
    assert!(all[6].body.is_empty());
    assert!(
        all.iter()
            .all(|request| request.path != "/api/download/authorize")
    );
}

#[cfg(unix)]
#[test]
fn video_download_reuses_finished_or_running_status_without_a_render_post() {
    for read_only in [false, true] {
        for running in [false, true] {
            let home = isolated_test_home("sunox-video-status-reuse");
            write_auth(&home);
            let bin = install_mock_video_tools(&home, true);
            let clip = serde_json::json!({"id":"video-clip","status":"complete","is_trashed":false,"title":"Video","model_name":"chirp-hawk","created_at":"","video_url":""}).to_string();
            let mut responses = vec![clip.clone()];
            if !read_only {
                responses.push(clip);
            }
            if running {
                responses.push(r#"{"status":"processing","video_url":""}"#.into());
            }
            // Invalid completed URL stops before transfer, proving no regeneration or fallback.
            responses.push(
                r#"{"status":"complete","video_url":"https://cdn.example/api/forbidden"}"#.into(),
            );
            let (base, requests) = serve_json_sequence(responses);
            let mut command = isolated_command(&home, &base);
            command
                .env("PATH", bin)
                .args(["download", "video-clip", "--video-timeout", "1", "--output"])
                .arg(home.join("output"))
                .args(["--quiet", "--json"]);
            if read_only {
                command.arg("--read-only");
            }
            command.assert().failure().stderr(predicates::str::contains(
                "no render or download authorization was submitted",
            ));
            let all: Vec<_> = requests.iter().collect();
            assert!(all.iter().all(|request| request.method == "GET"));
            assert_eq!(
                all.iter()
                    .filter(|request| request.path.ends_with("/status/"))
                    .count(),
                if running { 2 } else { 1 }
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn video_download_status_failure_and_unfinished_jobs_never_trigger_a_render() {
    for state in [r#"{"status":"future-unknown-state"}"#, r#"{"status":null}"#] {
        let home = isolated_test_home("sunox-video-status-refusal");
        write_auth(&home);
        let bin = install_mock_video_tools(&home, true);
        let clip = serde_json::json!({"id":"video-clip","status":"complete","is_trashed":false,"title":"Video","model_name":"chirp-hawk","created_at":"","video_url":""}).to_string();
        let (base, requests) =
            serve_json_sequence(vec![clip.clone(), clip, state.into(), state.into()]);
        isolated_command(&home, &base)
            .env("PATH", bin)
            .args([
                "download",
                "video-clip",
                "--video-timeout",
                "1",
                "--json",
                "--quiet",
                "--output",
            ])
            .arg(home.join("output"))
            .assert()
            .failure();
        assert!(requests.iter().all(|request| request.method == "GET"));
    }
}

#[cfg(unix)]
#[test]
fn retry_after_video_render_timeout_polls_existing_job_instead_of_submitting_again() {
    let home = isolated_test_home("sunox-video-timeout-retry");
    write_auth(&home);
    let bin = install_mock_video_tools(&home, true);
    let mut billing: serde_json::Value = serde_json::from_str(&new_mode_billing()).unwrap();
    billing["accessible_features"] = serde_json::json!([{"name":"generate_song_video"}]);
    let billing = billing.to_string();
    let clip = serde_json::json!({"id":"video-clip","user_id":"user-test","status":"complete","is_trashed":false,"title":"Video","model_name":"chirp-hawk","created_at":"","video_url":""}).to_string();
    let (base, requests) = serve_json_sequence(vec![
        clip.clone(),
        clip.clone(),
        r#"{"status":"complete","video_url":""}"#.into(),
        billing.clone(),
        clip.clone(),
        billing,
        "{}".into(),
        r#"{"status":"processing"}"#.into(),
        clip.clone(),
        clip,
        r#"{"status":"processing"}"#.into(),
        r#"{"status":"complete","video_url":"https://cdn.example/api/forbidden"}"#.into(),
    ]);
    for attempt in 0..2 {
        let assertion = isolated_command(&home, &base)
            .env("PATH", &bin)
            .args([
                "download",
                "video-clip",
                "--video-timeout",
                "1",
                "--json",
                "--quiet",
                "--output",
            ])
            .arg(home.join("output"))
            .assert()
            .failure();
        let error: serde_json::Value =
            serde_json::from_slice(&assertion.get_output().stderr).unwrap();
        assert_eq!(
            error["error"]["code"],
            if attempt == 0 {
                "ambiguous_mutation"
            } else {
                "download_error"
            }
        );
    }
    let all: Vec<_> = requests.iter().collect();
    assert_eq!(all.len(), 12);
    let posts: Vec<_> = all
        .iter()
        .filter(|request| request.method == "POST")
        .collect();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].path, "/api/video/generate/video-clip/");
}
