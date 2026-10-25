//! End-to-end tests of the compiled binary against a mock Yoto API: the
//! clispec contract, the exit-code envelope, sign-in state, content commands,
//! icons and the full upload flow.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mockito::{Matcher, Server};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_yoto-cli");
const ICON_ID: &str = "7Wt--qhx9t7PCSC8WBmGKJvlG53Z1uk7bN1dE9wGWJ0";
const DEFAULT_ICON: &str = "yoto:#aUm9i3ex3qqAMYBv-i-O-pYMKuMJGICtR3Vhf289u2Q";

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Output {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", self.stdout))
    }

    fn error(&self) -> Value {
        let last = self
            .stderr
            .lines()
            .last()
            .expect("stderr has an error line");
        serde_json::from_str::<Value>(last).expect("error envelope is JSON")["error"].clone()
    }
}

/// An isolated environment: its own token file and config dir, pointed at `api`.
struct Env {
    tmp: tempfile::TempDir,
    api: String,
}

impl Env {
    fn new(api: &str) -> Env {
        Env {
            tmp: tempfile::tempdir().unwrap(),
            api: api.to_string(),
        }
    }

    fn token_file(&self) -> PathBuf {
        self.tmp.path().join("tokens.json")
    }

    fn sign_in(&self, access: &str, expires_at: u64, refresh: Option<&str>) {
        let tokens = json!({
            "client_id": "client-1",
            "access_token": access,
            "refresh_token": refresh,
            "expires_at": expires_at,
            "scope": "user:content:manage offline_access",
        });
        std::fs::write(self.token_file(), tokens.to_string()).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(args, self.tmp.path())
    }

    fn command(&self, args: &[&str], cwd: &Path) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(cwd)
            .env("YOTO_CLI_API_URL", &self.api)
            .env("YOTO_CLI_AUTH_URL", &self.api)
            .env("YOTO_CLI_TOKEN_FILE", self.token_file())
            .env("YOTO_CLI_CONFIG_DIR", self.tmp.path().join("config"))
            .env_remove("YOTO_CLIENT_ID");
        cmd
    }

    fn run_in(&self, args: &[&str], cwd: &Path) -> Output {
        let out = self.command(args, cwd).output().expect("spawn binary");
        Output::from(out)
    }
}

impl From<std::process::Output> for Output {
    fn from(out: std::process::Output) -> Output {
        Output {
            code: out.status.code().unwrap(),
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
        }
    }
}

fn far_future() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600
}

fn png16() -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    b.extend_from_slice(&16u32.to_be_bytes());
    b.extend_from_slice(&16u32.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0]);
    b
}

/// A book folder with two MP3 tracks, a cover and an icons.json that gives
/// track 1 a custom icon file and track 2 a Yoto icon.
fn book(root: &Path) -> PathBuf {
    let dir = root.join("Boek");
    std::fs::create_dir_all(dir.join("icons")).unwrap();
    std::fs::write(dir.join("01 - Het begin.mp3"), b"ID3\x04\x00 first track").unwrap();
    std::fs::write(dir.join("02 - Het einde.mp3"), b"ID3\x04\x00 second track").unwrap();
    std::fs::write(dir.join("cover.png"), png16()).unwrap();
    std::fs::write(dir.join("icons/tractor.png"), png16()).unwrap();
    std::fs::write(
        dir.join("icons.json"),
        json!({"1": "icons/tractor.png", "2": format!("yoto:#{ICON_ID}")}).to_string(),
    )
    .unwrap();
    dir
}

#[test]
fn schema_is_clispec_v0_3() {
    let out = Env::new("http://127.0.0.1:9").run(&["schema"]);
    assert_eq!(out.code, 0);
    assert_eq!(out.json()["clispec"], "0.3");
}

#[test]
fn help_mentions_schema_and_unknown_flags_are_usage_errors() {
    let env = Env::new("http://127.0.0.1:9");
    let out = env.run(&["--help"]);
    assert_eq!(out.code, 0);
    assert!(out.stdout.contains("schema"));

    let out = env.run(&["content", "list", "--bogus"]);
    assert_eq!(out.code, 3);
    assert_eq!(out.error()["kind"], "usage");
}

#[test]
fn signed_out_status_is_not_an_error_but_commands_need_sign_in() {
    let env = Env::new("http://127.0.0.1:9");
    let out = env.run(&["auth", "status"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["signed_in"], false);

    let out = env.run(&["content", "list"]);
    assert_eq!(out.code, 4);
    assert_eq!(out.error()["kind"], "not_authenticated");
}

#[test]
fn logout_removes_the_token_file() {
    let env = Env::new("http://127.0.0.1:9");
    env.sign_in("a", far_future(), Some("r"));
    let out = env.run(&["auth", "logout"]);
    assert_eq!(out.code, 0);
    assert_eq!(out.json()["removed"], true);
    assert!(!env.token_file().exists());
    assert_eq!(env.run(&["auth", "logout"]).json()["removed"], false);
}

#[test]
fn login_without_any_client_id_is_a_usage_error() {
    let out = Env::new("http://127.0.0.1:9").run(&[
        "auth",
        "login",
        "--no-browser",
        "--port",
        "0",
        "--timeout",
        "1",
    ]);
    assert_eq!(out.code, 3);
    assert!(
        out.error()["message"]
            .as_str()
            .unwrap()
            .contains("dashboard.yoto.dev")
    );
}

#[test]
fn dry_run_shows_the_plan_with_icons_and_sends_nothing() {
    // Port 9 (discard) is never answered, so any request would fail the run.
    let env = Env::new("http://127.0.0.1:9");
    let dir = book(env.tmp.path());
    let out = env.run(&["upload", "--dry-run", dir.to_str().unwrap()]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["title"], "Boek");
    let tracks = v["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0]["title"], "Het begin");
    assert!(
        tracks[0]["icon"]
            .as_str()
            .unwrap()
            .ends_with("icons/tractor.png"),
        "{}",
        tracks[0]
    );
    assert_eq!(tracks[1]["icon"], format!("yoto:#{ICON_ID}"));
    assert!(v["icons_file"].as_str().unwrap().ends_with("icons.json"));

    let out = env.run(&["upload", "--dry-run", "--no-icons", dir.to_str().unwrap()]);
    assert_eq!(out.json()["tracks"][0]["icon"], Value::Null);
}

#[test]
fn upload_without_yes_needs_confirmation() {
    let env = Env::new("http://127.0.0.1:9");
    let dir = book(env.tmp.path());
    let out = env.run(&["upload", dir.to_str().unwrap()]);
    assert_eq!(out.code, 5);
    assert_eq!(out.error()["kind"], "confirmation_required");
}

#[test]
fn bad_icons_json_is_invalid_input() {
    let env = Env::new("http://127.0.0.1:9");
    let dir = book(env.tmp.path());
    std::fs::write(
        dir.join("icons.json"),
        json!({"3": format!("yoto:#{ICON_ID}")}).to_string(),
    )
    .unwrap();
    let out = env.run(&["upload", "--dry-run", dir.to_str().unwrap()]);
    assert_eq!(out.code, 1);
    assert_eq!(out.error()["kind"], "invalid_input");
}

#[test]
fn icon_overrides_bypass_a_broken_detected_icons_json() {
    let env = Env::new("http://127.0.0.1:9");
    let dir = book(env.tmp.path());
    std::fs::write(dir.join("icons.json"), "{ not json").unwrap();
    let replacement = env.tmp.path().join("other.json");
    std::fs::write(
        &replacement,
        json!({"2": format!("yoto:#{ICON_ID}")}).to_string(),
    )
    .unwrap();

    let out = env.run(&["upload", "--dry-run", "--no-icons", dir.to_str().unwrap()]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["tracks"][0]["icon"], Value::Null);
    assert_eq!(out.json()["icons_file"], Value::Null);

    let out = env.run(&[
        "upload",
        "--dry-run",
        "--icons",
        replacement.to_str().unwrap(),
        dir.to_str().unwrap(),
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["tracks"][0]["icon"], Value::Null);
    assert_eq!(v["tracks"][1]["icon"], format!("yoto:#{ICON_ID}"));
    assert!(v["icons_file"].as_str().unwrap().ends_with("other.json"));
}

#[test]
fn content_list_show_and_delete_against_the_api() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    env.sign_in("tok", far_future(), Some("r"));
    let card = json!({"cardId": "abc12", "title": "Boer Boris", "metadata": {"media": {"duration": 125.0}},
        "content": {"chapters": [
            {"key": "01", "title": "Een", "display": {"icon16x16": "yoto:#tractorIcon"},
             "tracks": [{"key": "01", "title": "Een", "duration": 125.0, "format": "aac", "type": "audio"}]},
            {"key": "02", "title": "Twee", "tracks": []}]}});

    let list = server
        .mock("GET", "/content/mine")
        .match_header("authorization", "Bearer tok")
        .with_body(json!({"cards": [card]}).to_string())
        .create();
    let out = env.run(&["content", "list"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["items"][0]["card_id"], "abc12");
    assert_eq!(out.json()["items"][0]["duration_seconds"], 125.0);
    list.assert();

    let show = server
        .mock("GET", "/content/abc12")
        .with_body(json!({"card": card}).to_string())
        .expect_at_least(1)
        .create();
    let out = env.run(&["content", "show", "abc12"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.json()["chapters"][0]["tracks"][0]["duration_seconds"],
        125.0
    );
    assert_eq!(out.json()["chapters"][0]["icon"], "yoto:#tractorIcon");
    assert_eq!(out.json()["chapters"][1]["icon"], Value::Null);

    // Expect exactly one DELETE across both runs: the one without --yes sends none.
    let delete = server
        .mock("DELETE", "/content/abc12")
        .with_body("{}")
        .expect(1)
        .create();
    let out = env.run(&["content", "delete", "abc12"]);
    assert_eq!(out.code, 5, "{}", out.stderr);
    assert_eq!(out.error()["kind"], "confirmation_required");
    let out = env.run(&["content", "delete", "abc12", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["deleted"], true);
    delete.assert();
    show.assert();
}

#[test]
fn missing_content_is_not_found() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    env.sign_in("tok", far_future(), None);
    server
        .mock("GET", "/content/nope")
        .with_status(404)
        .with_body(r#"{"error":{"message":"not found"}}"#)
        .create();
    let out = env.run(&["content", "show", "nope"]);
    assert_eq!(out.code, 7, "{}", out.stderr);
    assert_eq!(out.error()["kind"], "not_found");
}

#[test]
fn icons_list_filters_on_every_word() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    env.sign_in("tok", far_future(), None);
    server
        .mock("GET", "/media/displayIcons/user/yoto")
        .with_body(
            json!({"displayIcons": [
                {"mediaId": ICON_ID, "title": "Red Tractor", "publicTags": ["farm", "vehicle"], "url": "https://example.com/t.png"},
                {"mediaId": "x", "title": "Cat", "publicTags": ["animal", "farm"]},
            ]})
            .to_string(),
        )
        .create();
    let out = env.run(&["icons", "list", "-q", "farm", "tractor"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["total"], 2);
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    assert_eq!(v["items"][0]["icon"], format!("yoto:#{ICON_ID}"));
}

/// Mocks for one track: an upload slot (with or without a PUT URL) and a
/// finished transcode.
fn mock_track(server: &mut Server, sha: &str, n: usize, needs_upload: bool) -> Vec<mockito::Mock> {
    let upload_url = needs_upload.then(|| format!("{}/put/{n}", server.url()));
    let mut mocks = vec![
        server
            .mock("GET", "/media/transcode/audio/uploadUrl")
            .match_query(Matcher::UrlEncoded("sha256".into(), sha.into()))
            .with_body(json!({"upload": {"uploadId": format!("up{n}"), "uploadUrl": upload_url}}).to_string())
            .expect(1)
            .create(),
        server
            .mock("GET", format!("/media/upload/up{n}/transcoded").as_str())
            .match_query(Matcher::Any)
            .with_body(
                json!({"transcode": {"transcodedSha256": format!("t{n}"), "transcodedInfo": {"duration": 60.0 * n as f64, "fileSize": 1000 * n, "channels": "stereo", "format": "aac"}}})
                    .to_string(),
            )
            .expect(1)
            .create(),
    ];
    if needs_upload {
        mocks.push(
            server
                .mock("PUT", format!("/put/{n}").as_str())
                .match_header("authorization", Matcher::Missing)
                .match_header("content-type", "audio/mpeg")
                .expect(1)
                .create(),
        );
    }
    mocks
}

/// Answer `POST /content` with `card_id` and keep the request body for inspection.
fn mock_create(
    server: &mut Server,
    partial: Value,
    card_id: &str,
) -> (mockito::Mock, Arc<Mutex<Vec<Value>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let reply = json!({"card": {"cardId": card_id}})
        .to_string()
        .into_bytes();
    let mock = server
        .mock("POST", "/content")
        .match_body(Matcher::PartialJson(partial))
        .with_body_from_request(move |req| {
            sink.lock()
                .unwrap()
                .push(serde_json::from_slice(req.body().unwrap()).unwrap());
            reply.clone()
        })
        .expect(1)
        .create();
    (mock, seen)
}

fn sha256_hex(path: &Path) -> String {
    use std::fmt::Write;
    let digest = <sha2::Sha256 as sha2::Digest>::digest(std::fs::read(path).unwrap());
    digest.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[test]
fn upload_sends_cover_icons_and_audio_then_creates_the_playlist() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    env.sign_in("tok", far_future(), None);
    let dir = book(env.tmp.path());
    let sha1 = sha256_hex(&dir.join("01 - Het begin.mp3"));
    let sha2 = sha256_hex(&dir.join("02 - Het einde.mp3"));

    let cover = server
        .mock("POST", "/media/coverImage/user/me/upload")
        .match_query(Matcher::Any)
        .match_header("content-type", "image/png")
        .with_body(
            json!({"coverImage": {"mediaUrl": "https://cdn.example.com/cover.png"}}).to_string(),
        )
        .expect(1)
        .create();
    let icon = server
        .mock("POST", "/media/displayIcons/user/me/upload")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("autoConvert".into(), "false".into()),
            Matcher::UrlEncoded("filename".into(), "tractor".into()),
        ]))
        .match_header("content-type", "image/png")
        .match_body(png16())
        .with_body(json!({"displayIcon": {"mediaId": "uploadedTractorIcon"}}).to_string())
        .expect(1)
        .create();
    // Track 1 is new; Yoto already has track 2 (no upload URL).
    let t1 = mock_track(&mut server, &sha1, 1, true);
    let t2 = mock_track(&mut server, &sha2, 2, false);
    let (create, seen) = mock_create(
        &mut server,
        json!({
            "title": "Boek",
            "metadata": {"cover": {"imageL": "https://cdn.example.com/cover.png"}, "media": {"duration": 180.0, "fileSize": 3000}},
        }),
        "new01",
    );

    let out = env.run(&["upload", "--yes", dir.to_str().unwrap()]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["card_id"], "new01");
    assert_eq!(v["uploaded"], 1);
    assert_eq!(v["reused"], 1);
    assert_eq!(v["custom_icons"], 2);
    assert_eq!(v["replaced"], false);
    for m in [&cover, &icon, &create].into_iter().chain(&t1).chain(&t2) {
        m.assert();
    }

    // Each chapter carries the icon assigned to its track.
    let body = seen.lock().unwrap()[0].clone();
    let chapters = body["content"]["chapters"].as_array().unwrap();
    assert_eq!(
        chapters[0]["display"]["icon16x16"],
        "yoto:#uploadedTractorIcon"
    );
    assert_eq!(
        chapters[0]["tracks"][0]["display"]["icon16x16"],
        "yoto:#uploadedTractorIcon"
    );
    assert_eq!(
        chapters[1]["display"]["icon16x16"],
        format!("yoto:#{ICON_ID}")
    );
    assert_eq!(chapters[1]["tracks"][0]["trackUrl"], "yoto:#t2");
    assert!(body.get("cardId").is_none(), "a new playlist has no cardId");
}

#[test]
fn replace_checks_the_playlist_first_and_sends_its_card_id() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    env.sign_in("tok", far_future(), None);
    let dir = book(env.tmp.path());

    // Only the user's own playlists can be replaced. A missing playlist, and
    // one Yoto lets the user read but not edit, stop the run before any upload.
    server
        .mock("GET", "/content/mine")
        .with_body(json!({"cards": [{"cardId": "old01", "title": "Oud"}]}).to_string())
        .create();
    server
        .mock("GET", "/content/gone1")
        .with_status(404)
        .with_body("{}")
        .create();
    server
        .mock("GET", "/content/read1")
        .with_body(json!({"card": {"cardId": "read1"}}).to_string())
        .create();
    let untouched = server
        .mock("GET", "/media/transcode/audio/uploadUrl")
        .match_query(Matcher::Any)
        .expect(0)
        .create();
    for id in ["gone1", "read1"] {
        let out = env.run(&[
            "upload",
            "--yes",
            "--no-cover",
            "--no-icons",
            "--replace",
            id,
            dir.to_str().unwrap(),
        ]);
        assert_eq!(out.code, 7, "{id}: {}", out.stderr);
        assert!(out.stderr.contains(id), "{id}: {}", out.stderr);
    }
    untouched.assert();
    untouched.remove();

    server
        .mock("GET", "/content/old01")
        .with_body(json!({"card": {"cardId": "old01"}}).to_string())
        .create();
    let _t1 = mock_track(
        &mut server,
        &sha256_hex(&dir.join("01 - Het begin.mp3")),
        1,
        false,
    );
    let _t2 = mock_track(
        &mut server,
        &sha256_hex(&dir.join("02 - Het einde.mp3")),
        2,
        false,
    );
    let (create, seen) = mock_create(&mut server, json!({"cardId": "old01"}), "old01");
    let out = env.run(&[
        "upload",
        "--yes",
        "--no-cover",
        "--no-icons",
        "--replace",
        "old01",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["replaced"], true);
    assert_eq!(out.json()["custom_icons"], 0);
    create.assert();
    let body = seen.lock().unwrap()[0].clone();
    assert_eq!(
        body["content"]["chapters"][0]["display"]["icon16x16"],
        DEFAULT_ICON
    );
    assert!(body["metadata"].get("cover").is_none());
}

#[test]
fn expired_token_is_refreshed_and_the_rotated_one_persisted() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    env.sign_in("old", 1, Some("refresh-1"));
    let token = server
        .mock("POST", "/oauth/token")
        .match_body(Matcher::AllOf(vec![
            Matcher::UrlEncoded("grant_type".into(), "refresh_token".into()),
            Matcher::UrlEncoded("refresh_token".into(), "refresh-1".into()),
            Matcher::UrlEncoded("client_id".into(), "client-1".into()),
        ]))
        .with_body(
            json!({"access_token": "new", "refresh_token": "refresh-2", "expires_in": 86400})
                .to_string(),
        )
        .expect(1)
        .create();
    server
        .mock("GET", "/content/mine")
        .match_header("authorization", "Bearer new")
        .with_body(r#"{"cards":[]}"#)
        .create();

    let out = env.run(&["content", "list"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    token.assert();
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(env.token_file()).unwrap()).unwrap();
    assert_eq!(stored["access_token"], "new");
    assert_eq!(stored["refresh_token"], "refresh-2");
    assert_eq!(
        stored["scope"], "user:content:manage offline_access",
        "scope survives a refresh that omits it"
    );
}

#[test]
fn refresh_without_rotation_keeps_the_refresh_token() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    env.sign_in("old", 1, Some("refresh-1"));
    server
        .mock("POST", "/oauth/token")
        .with_body(json!({"access_token": "new", "expires_in": 86400}).to_string())
        .create();
    server
        .mock("GET", "/content/mine")
        .with_body(r#"{"cards":[]}"#)
        .create();
    assert_eq!(env.run(&["content", "list"]).code, 0);
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(env.token_file()).unwrap()).unwrap();
    assert_eq!(stored["refresh_token"], "refresh-1");
}

/// A token endpoint that rotates `refresh-1` into access token `new`, once.
fn mock_rotation(server: &mut Server) -> mockito::Mock {
    server
        .mock("POST", "/oauth/token")
        .match_body(Matcher::UrlEncoded(
            "refresh_token".into(),
            "refresh-1".into(),
        ))
        .with_body(
            json!({"access_token": "new", "refresh_token": "refresh-2", "expires_in": 86400})
                .to_string(),
        )
        .expect(1)
        .create()
}

#[test]
fn concurrent_commands_spend_the_refresh_token_once() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    env.sign_in("old", 1, Some("refresh-1"));
    let (token, _) = mock_slow_rotation(&mut server);
    server
        .mock("GET", "/content/mine")
        .match_header("authorization", "Bearer new")
        .with_body(r#"{"cards":[]}"#)
        .expect(4)
        .create();
    list_four_at_once(&env);
    token.assert();
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(env.token_file()).unwrap()).unwrap();
    assert_eq!(stored["refresh_token"], "refresh-2");
}

#[test]
fn concurrent_commands_rejected_together_renew_once() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    env.sign_in("old", far_future(), Some("refresh-1"));
    server
        .mock("GET", "/content/mine")
        .match_header("authorization", "Bearer old")
        .with_status(401)
        .create();
    let (token, _) = mock_slow_rotation(&mut server);
    server
        .mock("GET", "/content/mine")
        .match_header("authorization", "Bearer new")
        .with_body(r#"{"cards":[]}"#)
        .expect(4)
        .create();
    list_four_at_once(&env);
    token.assert();
}

#[test]
fn logout_during_a_refresh_stays_signed_out() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    env.sign_in("old", 1, Some("refresh-1"));
    let (_, refreshing) = mock_slow_rotation(&mut server);
    server
        .mock("GET", "/content/mine")
        .with_body(r#"{"cards":[]}"#)
        .create();
    let list = env
        .command(&["content", "list"], env.tmp.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn binary");
    while !refreshing.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let out = env.run(&["auth", "logout"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let _ = list.wait_with_output().unwrap();
    assert!(
        !env.token_file().exists(),
        "the refresh in flight signed the account back in"
    );
}

/// Run `content list` four times in parallel; every run must succeed.
fn list_four_at_once(env: &Env) {
    let children: Vec<_> = (0..4)
        .map(|_| {
            env.command(&["content", "list"], env.tmp.path())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn binary")
        })
        .collect();
    for child in children {
        let out = Output::from(child.wait_with_output().unwrap());
        assert_eq!(out.code, 0, "{}", out.stderr);
    }
}

/// Like [`mock_rotation`], but slow: the first refresh is still in flight
/// while the other commands start, so they all hold the same sign-in.
fn mock_slow_rotation(server: &mut Server) -> (mockito::Mock, Arc<AtomicBool>) {
    let started = Arc::new(AtomicBool::new(false));
    let flag = started.clone();
    let mock = server
        .mock("POST", "/oauth/token")
        .match_body(Matcher::UrlEncoded(
            "refresh_token".into(),
            "refresh-1".into(),
        ))
        .with_body_from_request(move |_| {
            flag.store(true, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(500));
            json!({"access_token": "new", "refresh_token": "refresh-2", "expires_in": 86400})
                .to_string()
                .into()
        })
        .expect(1)
        .create();
    (mock, started)
}

#[cfg(unix)]
#[test]
fn saved_tokens_stay_private_whatever_sits_beside_the_token_file() {
    use std::os::unix::fs::PermissionsExt;
    let mut server = Server::new();
    let env = Env::new(&server.url());
    server
        .mock("POST", "/oauth/token")
        .with_body(
            json!({"access_token": "new", "refresh_token": "refresh-2", "expires_in": 86400})
                .to_string(),
        )
        .create();
    server
        .mock("GET", "/content/mine")
        .with_body(r#"{"cards":[]}"#)
        .create();
    let refresh_and_save = || {
        env.sign_in("old", 1, Some("refresh-1"));
        let out = env.run(&["content", "list"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(env.token_file()).unwrap()).unwrap();
        assert_eq!(stored["access_token"], "new");
        let mode = std::fs::metadata(env.token_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "token file mode {mode:o}");
    };
    // Planted where a writer with a fixed temporary name would open it.
    let planted = env.tmp.path().join("tokens.tmp");

    std::fs::write(&planted, "").unwrap();
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o644)).unwrap();
    refresh_and_save();

    let _ = std::fs::remove_file(&planted);
    let victim = env.tmp.path().join("victim");
    std::fs::write(&victim, "untouched").unwrap();
    std::os::unix::fs::symlink(&victim, &planted).unwrap();
    refresh_and_save();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
}

#[test]
fn token_rejected_mid_upload_is_renewed_and_the_run_finishes() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    // Looks valid for an hour, but Yoto stops accepting it partway through
    // the run, as happens when a long upload outlives the access token.
    env.sign_in("old", far_future(), Some("refresh-1"));
    let dir = book(env.tmp.path());
    let _t1 = mock_track(
        &mut server,
        &sha256_hex(&dir.join("01 - Het begin.mp3")),
        1,
        false,
    );
    let _t2 = mock_track(
        &mut server,
        &sha256_hex(&dir.join("02 - Het einde.mp3")),
        2,
        false,
    );
    let rejected = server
        .mock("POST", "/content")
        .match_header("authorization", "Bearer old")
        .with_status(401)
        .with_body(r#"{"error":{"message":"jwt expired"}}"#)
        .expect(1)
        .create();
    let rotation = mock_rotation(&mut server);
    let created = server
        .mock("POST", "/content")
        .match_header("authorization", "Bearer new")
        .with_body(json!({"card": {"cardId": "new01"}}).to_string())
        .expect(1)
        .create();

    let out = env.run(&[
        "upload",
        "--yes",
        "--no-cover",
        "--no-icons",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["card_id"], "new01");
    for m in [&rejected, &rotation, &created] {
        m.assert();
    }
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(env.token_file()).unwrap()).unwrap();
    assert_eq!(stored["access_token"], "new");
    assert_eq!(stored["refresh_token"], "refresh-2");
}

/// A book whose first poll is refused with a 401, so the run has to renew
/// the sign-in while it waits for the transcode.
fn poll_needs_renewal(server: &mut Server, env: &Env) -> PathBuf {
    env.sign_in("old", far_future(), Some("refresh-1"));
    let dir = book(env.tmp.path());
    server
        .mock("GET", "/media/transcode/audio/uploadUrl")
        .match_query(Matcher::Any)
        .with_body(json!({"upload": {"uploadId": "up1", "uploadUrl": null}}).to_string())
        .create();
    server
        .mock("GET", "/media/upload/up1/transcoded")
        .match_query(Matcher::Any)
        .match_header("authorization", "Bearer old")
        .with_status(401)
        .create();
    server
        .mock("GET", "/media/upload/up1/transcoded")
        .match_query(Matcher::Any)
        .match_header("authorization", "Bearer new")
        .with_status(404)
        .create();
    dir
}

fn upload_with_one_second_transcode(env: &Env, dir: &Path) -> (Output, Duration) {
    let started = Instant::now();
    let out = env.run(&[
        "upload",
        "--yes",
        "--no-cover",
        "--no-icons",
        "--transcode-timeout",
        "1",
        dir.to_str().unwrap(),
    ]);
    (out, started.elapsed())
}

#[test]
fn a_stalled_token_refresh_cannot_hold_the_transcode_past_its_deadline() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    let dir = poll_needs_renewal(&mut server, &env);
    server
        .mock("POST", "/oauth/token")
        .with_body_from_request(|_| {
            std::thread::sleep(Duration::from_secs(6));
            json!({"access_token": "new", "refresh_token": "refresh-2", "expires_in": 86400})
                .to_string()
                .into()
        })
        .create();

    let (out, elapsed) = upload_with_one_second_transcode(&env, &dir);
    assert_eq!(out.code, 10, "{}", out.stderr);
    assert_eq!(out.error()["kind"], "transcode_timeout");
    assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
}

#[test]
fn a_held_token_lock_cannot_hold_the_transcode_past_its_deadline() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    let dir = poll_needs_renewal(&mut server, &env);
    let _rotation = mock_rotation(&mut server);
    // Another yoto-cli process is refreshing and keeps the lock for 6 s.
    let mut lock_path = env.token_file().into_os_string();
    lock_path.push(".lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .unwrap();
    lock.lock().unwrap();
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(6));
        drop(lock);
    });

    let (out, elapsed) = upload_with_one_second_transcode(&env, &dir);
    holder.join().unwrap();
    assert_eq!(out.code, 10, "{}", out.stderr);
    assert_eq!(out.error()["kind"], "transcode_timeout");
    assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
}

#[test]
fn token_still_rejected_after_renewal_means_sign_in_again() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    env.sign_in("old", far_future(), Some("refresh-1"));
    let rotation = mock_rotation(&mut server);
    let listing = server
        .mock("GET", "/content/mine")
        .with_status(401)
        .with_body(r#"{"error":{"message":"revoked"}}"#)
        .expect(2)
        .create();
    let out = env.run(&["content", "list"]);
    assert_eq!(out.code, 4, "{}", out.stderr);
    assert_eq!(out.error()["kind"], "not_authenticated");
    rotation.assert();
    listing.assert();
}

#[test]
fn rejected_refresh_means_sign_in_again() {
    let mut server = Server::new();
    let env = Env::new(&server.url());
    env.sign_in("old", 1, Some("spent"));
    server
        .mock("POST", "/oauth/token")
        .with_status(403)
        .with_body(
            r#"{"error":"invalid_grant","error_description":"Unknown or invalid refresh token."}"#,
        )
        .create();
    let out = env.run(&["content", "list"]);
    assert_eq!(out.code, 4, "{}", out.stderr);
    assert_eq!(out.error()["kind"], "not_authenticated");
}

#[test]
fn every_error_envelope_kind_is_declared_with_its_exit_code() {
    let env = Env::new("http://127.0.0.1:9");
    let schema = env.run(&["schema"]).json();
    let declared = schema.to_string();
    // A sample of real failures: each envelope's kind and exit code must
    // match the schema's declaration.
    let dir = book(env.tmp.path());
    let cases = [
        env.run(&["content", "list", "--bogus"]),
        env.run(&["content", "list"]),
        env.run(&["upload", dir.to_str().unwrap()]),
        env.run(&["upload", "--dry-run", "/nonexistent/dir"]),
    ];
    for out in cases {
        let err = out.error();
        let kind = err["kind"].as_str().unwrap();
        assert_eq!(err["exit_code"], out.code, "{kind}");
        assert!(
            declared.contains(&format!("\"{kind}\"")),
            "{kind} is not declared"
        );
    }
}
