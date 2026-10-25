//! OAuth 2.0 Authorization Code + PKCE sign-in with a loopback redirect, and
//! token refresh.
//!
//! Yoto supports only public clients, so there is no client secret: the PKCE
//! verifier, generated fresh per login and never stored, is what proves the
//! code exchange comes from the process that started the login.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Url;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::{AUDIENCE, Endpoints, TokenStore, Tokens};
use crate::error::{Error, Result};

/// Scopes requested at login: read and manage MYO content, upload icons, and
/// stay signed in between runs. Yoto checks each scope literally, so viewing
/// is requested on its own even though managing implies it on the dashboard.
pub const DEFAULT_SCOPE: &str =
    "user:content:view user:content:manage user:icons:manage offline_access";
/// Refresh this long before the access token actually expires.
const EXPIRY_MARGIN_SECS: u64 = 30;

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn random_urlsafe(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("OS random source available");
    URL_SAFE_NO_PAD.encode(buf)
}

/// A PKCE verifier and its S256 challenge.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Self {
        Self::from_verifier(random_urlsafe(32))
    }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Pkce {
            verifier,
            challenge,
        }
    }
}

pub fn new_state() -> String {
    random_urlsafe(16)
}

pub fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/callback")
}

pub fn authorize_url(
    endpoints: &Endpoints,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    pkce: &Pkce,
    state: &str,
) -> String {
    Url::parse_with_params(
        &format!("{}/authorize", endpoints.auth),
        &[
            ("audience", AUDIENCE),
            ("scope", scope),
            ("response_type", "code"),
            ("client_id", client_id),
            ("code_challenge", pkce.challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("redirect_uri", redirect_uri),
            ("state", state),
        ],
    )
    .expect("authorize URL is valid")
    .to_string()
}

/// Bind the loopback listener before the browser is opened, so a busy port
/// fails fast instead of after the user signed in.
pub fn bind_callback(port: u16) -> Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port)).map_err(|e| Error::AuthFailed {
        message: format!(
            "cannot listen on 127.0.0.1:{port} for the sign-in redirect: {e} (choose another with --port and register it as a redirect URI)"
        ),
    })
}

/// Wait for the browser to hit `/callback` and return the authorization code.
///
/// Other paths (a browser's `/favicon.ico`) get a 404 and are ignored. The
/// `state` must match, which ties the redirect to this login attempt.
pub fn wait_for_code(
    listener: &TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| auth_failed(format!("cannot poll the callback listener: {e}")))?;
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(result) = handle_callback(stream, expected_state) {
                    return result;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(auth_failed(format!(
                        "no sign-in redirect within {} seconds",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(auth_failed(format!("callback listener failed: {e}"))),
        }
    }
}

fn auth_failed(message: String) -> Error {
    Error::AuthFailed { message }
}

/// Handle one connection. `None` means it was not the callback; keep waiting.
fn handle_callback(mut stream: TcpStream, expected_state: &str) -> Option<Result<String>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() {
        return None;
    }
    let target = line.split_whitespace().nth(1).unwrap_or("");
    let url = match Url::parse(&format!("http://127.0.0.1{target}")) {
        Ok(u) if u.path() == "/callback" => u,
        _ => {
            respond(&mut stream, "404 Not Found", "Not found.");
            return None;
        }
    };
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    let result = if let Some(error) = param("error") {
        let detail = param("error_description").unwrap_or_default();
        Err(auth_failed(
            format!("sign-in was refused: {error} {detail}")
                .trim_end()
                .to_string(),
        ))
    } else if param("state").as_deref() != Some(expected_state) {
        Err(auth_failed(
            "sign-in redirect carried the wrong state, so it does not belong to this login".into(),
        ))
    } else if let Some(code) = param("code") {
        Ok(code)
    } else {
        Err(auth_failed(
            "sign-in redirect had no authorization code".into(),
        ))
    };
    let body = match &result {
        Ok(_) => "Signed in. You can close this window and return to the terminal.",
        Err(_) => "Sign-in failed. Return to the terminal for details.",
    };
    respond(&mut stream, "200 OK", body);
    Some(result)
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>yoto-cli</title><p style=\"font-family:system-ui;margin:3em\">{body}</p>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.flush();
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

fn token_request(
    http: &reqwest::blocking::Client,
    endpoints: &Endpoints,
    form: &[(&str, &str)],
    deadline: Option<Instant>,
) -> std::result::Result<TokenResponse, (u16, String, String)> {
    let url = format!("{}/oauth/token", endpoints.auth);
    let mut rb = http.post(url).form(form);
    if let Some(deadline) = deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        rb = rb.timeout(left.max(Duration::from_millis(1)));
    }
    let resp = rb
        .send()
        .map_err(|e| (0, "network".to_string(), e.to_string()))?;
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    if (200..300).contains(&status) {
        return serde_json::from_str(&text)
            .map_err(|e| (status, "invalid_response".to_string(), e.to_string()));
    }
    match serde_json::from_str::<OAuthError>(&text) {
        Ok(err) => Err((status, err.error, err.error_description.unwrap_or_default())),
        Err(_) => Err((status, "http_error".to_string(), truncate(&text, 200))),
    }
}

fn tokens_from(resp: TokenResponse, client_id: &str, previous_refresh: Option<String>) -> Tokens {
    let expires_at = resp
        .expires_in
        .map(|s| now_unix() + s)
        .or_else(|| jwt_exp(&resp.access_token));
    Tokens {
        client_id: client_id.to_string(),
        expires_at,
        refresh_token: resp.refresh_token.or(previous_refresh),
        scope: resp.scope,
        access_token: resp.access_token,
    }
}

pub fn exchange_code(
    http: &reqwest::blocking::Client,
    endpoints: &Endpoints,
    client_id: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<Tokens> {
    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code_verifier", verifier),
        ("code", code),
        ("redirect_uri", redirect_uri),
    ];
    match token_request(http, endpoints, &form, None) {
        Ok(resp) => Ok(tokens_from(resp, client_id, None)),
        Err((0, _, msg)) => Err(Error::Network {
            message: format!("token exchange failed: {msg}"),
        }),
        Err((status, code, msg)) => Err(auth_failed(
            format!("token exchange was rejected ({status} {code}): {msg}")
                .trim_end()
                .to_string(),
        )),
    }
}

/// Exchange the refresh token for new tokens, giving up once `deadline`
/// passes. The caller must persist the result immediately: the old refresh
/// token is now spent.
pub fn refresh(
    http: &reqwest::blocking::Client,
    endpoints: &Endpoints,
    tokens: &Tokens,
    deadline: Option<Instant>,
) -> Result<Tokens> {
    let Some(refresh_token) = tokens.refresh_token.as_deref() else {
        return Err(Error::NotAuthenticated {
            message: "the access token expired and there is no refresh token".into(),
        });
    };
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", tokens.client_id.as_str()),
        ("refresh_token", refresh_token),
    ];
    match token_request(http, endpoints, &form, deadline) {
        Ok(resp) => {
            // A server that does not rotate refresh tokens omits the new one;
            // the current one then stays valid and must be kept.
            let mut fresh = tokens_from(resp, &tokens.client_id, Some(refresh_token.to_string()));
            if fresh.scope.is_none() {
                fresh.scope = tokens.scope.clone();
            }
            Ok(fresh)
        }
        Err((0, _, msg)) => Err(Error::Network {
            message: format!("token refresh failed: {msg}"),
        }),
        Err((status, code, msg)) if status == 400 || status == 401 || status == 403 => {
            Err(Error::NotAuthenticated {
                message: format!("the sign-in is no longer valid ({code}): {msg}")
                    .trim_end()
                    .to_string(),
            })
        }
        Err((status, code, msg)) => Err(Error::Api {
            status,
            message: format!("token refresh failed ({code}): {msg}")
                .trim_end()
                .to_string(),
        }),
    }
}

/// Whether the access token is still usable for at least the safety margin.
pub fn is_fresh(tokens: &Tokens, now: u64) -> bool {
    tokens
        .expires_at
        .is_some_and(|exp| exp > now + EXPIRY_MARGIN_SECS)
}

/// The stored sign-in, handed to the API client so it can renew the access
/// token in the middle of a long run. Every refresh is persisted at once,
/// because it spends the previous refresh token, and happens under the
/// store's lock, so concurrent commands spend it once: whoever waited for
/// the lock re-reads the store and takes the token the other one saved.
pub struct StoredCredentials {
    pub store: TokenStore,
    pub http: reqwest::blocking::Client,
    pub endpoints: Endpoints,
}

impl StoredCredentials {
    fn load(&self) -> Result<Tokens> {
        self.store.load()?.ok_or_else(|| Error::NotAuthenticated {
            message: "not signed in".into(),
        })
    }

    /// Under the lock: the stored tokens if `usable` accepts them, else a
    /// refresh of them. Waiting for the lock and the refresh both stop at
    /// `deadline`.
    fn refresh_unless(
        &self,
        usable: impl Fn(&Tokens) -> bool,
        deadline: Option<Instant>,
    ) -> Result<Tokens> {
        let _lock = self.store.lock_until(deadline)?;
        let tokens = self.load()?;
        if usable(&tokens) {
            return Ok(tokens);
        }
        let fresh = refresh(&self.http, &self.endpoints, &tokens, deadline)?;
        self.store.save(&fresh)?;
        Ok(fresh)
    }
}

impl crate::api::Credentials for StoredCredentials {
    fn current(&self, deadline: Option<Instant>) -> Result<Tokens> {
        let tokens = self.load()?;
        if is_fresh(&tokens, now_unix()) {
            return Ok(tokens);
        }
        self.refresh_unless(|t| is_fresh(t, now_unix()), deadline)
    }

    fn renew(&self, rejected: &Tokens, deadline: Option<Instant>) -> Result<Tokens> {
        self.refresh_unless(
            |t| t.access_token != rejected.access_token && is_fresh(t, now_unix()),
            deadline,
        )
    }
}

/// The `exp` claim of a JWT, without verifying the signature (the API does that).
pub fn jwt_exp(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("exp")?.as_u64()
}

pub(crate) fn truncate(text: &str, max: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}...", &text[..i]),
        None => text.to_string(),
    }
}

/// Try to open the sign-in page in the default browser. Failure is fine: the
/// URL is always printed too.
pub fn open_browser(url: &str) -> bool {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_is_43_urlsafe_chars() {
        let pkce = Pkce::generate();
        assert_eq!(pkce.verifier.len(), 43);
        assert!(
            pkce.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(pkce.verifier, Pkce::generate().verifier);
    }

    #[test]
    fn authorize_url_carries_every_parameter() {
        let endpoints = Endpoints {
            api: "https://api.example".into(),
            auth: "https://login.example".into(),
        };
        let pkce = Pkce::from_verifier("v".into());
        // Yoto checks each scope literally: `user:content:manage` does not
        // grant reading a playlist, so every scope the commands use is named.
        let mut scopes: Vec<&str> = DEFAULT_SCOPE.split(' ').collect();
        scopes.sort_unstable();
        assert_eq!(
            scopes,
            [
                "offline_access",
                "user:content:manage",
                "user:content:view",
                "user:icons:manage"
            ]
        );
        let url = Url::parse(&authorize_url(
            &endpoints,
            "cid",
            "http://127.0.0.1:8787/callback",
            DEFAULT_SCOPE,
            &pkce,
            "st",
        ))
        .unwrap();
        assert_eq!(url.path(), "/authorize");
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["audience"], AUDIENCE);
        assert_eq!(q["scope"], DEFAULT_SCOPE);
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "cid");
        assert_eq!(q["code_challenge"], pkce.challenge);
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:8787/callback");
        assert_eq!(q["state"], "st");
    }

    #[test]
    fn jwt_exp_reads_the_claim_and_rejects_garbage() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"exp":1700000000,"sub":"x"}"#);
        assert_eq!(jwt_exp(&format!("h.{payload}.s")), Some(1_700_000_000));
        assert_eq!(jwt_exp("not-a-jwt"), None);
        let no_exp = URL_SAFE_NO_PAD.encode(br#"{"sub":"x"}"#);
        assert_eq!(jwt_exp(&format!("h.{no_exp}.s")), None);
    }

    #[test]
    fn freshness_uses_margin_and_treats_unknown_expiry_as_expired() {
        let mut t = Tokens {
            client_id: "c".into(),
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Some(1000),
            scope: None,
        };
        assert!(is_fresh(&t, 900));
        assert!(!is_fresh(&t, 980));
        t.expires_at = None;
        assert!(!is_fresh(&t, 0));
    }

    fn callback(listener_port: u16, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", listener_port)).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        let mut out = String::new();
        std::io::Read::read_to_string(&mut s, &mut out).unwrap();
        out
    }

    #[test]
    fn callback_ignores_other_paths_and_returns_code() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::thread::spawn(move || {
            let favicon = callback(port, "/favicon.ico");
            let ok = callback(port, "/callback?code=abc%2F1&state=s1");
            (favicon, ok)
        });
        let code = wait_for_code(&listener, "s1", Duration::from_secs(10)).unwrap();
        let (favicon, ok) = client.join().unwrap();
        assert_eq!(code, "abc/1");
        assert!(favicon.starts_with("HTTP/1.1 404"));
        assert!(ok.contains("Signed in"));
    }

    #[test]
    fn callback_rejects_wrong_state_and_reports_refusal() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let t = std::thread::spawn(move || callback(port, "/callback?code=abc&state=evil"));
        let err = wait_for_code(&listener, "s1", Duration::from_secs(10)).unwrap_err();
        t.join().unwrap();
        assert_eq!(err.kind(), "auth_failed");
        assert!(err.to_string().contains("wrong state"));

        let t = std::thread::spawn(move || {
            callback(
                port,
                "/callback?error=access_denied&error_description=nope&state=s1",
            )
        });
        let err = wait_for_code(&listener, "s1", Duration::from_secs(10)).unwrap_err();
        t.join().unwrap();
        assert!(err.to_string().contains("access_denied nope"));
    }

    #[test]
    fn callback_times_out() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let err = wait_for_code(&listener, "s", Duration::from_millis(250)).unwrap_err();
        assert_eq!(err.kind(), "auth_failed");
    }
}
