//! A small blocking client for the Yoto content and media endpoints.
//!
//! Only idempotent requests are retried, and only on 429 and 5xx gateway
//! errors, a few times with backoff that honours `Retry-After`. Creating
//! content is never retried: a lost response there could mean a duplicate
//! playlist.
//!
//! A signed-in client renews its access token shortly before it expires,
//! and once more if the API rejects it with 401, so an upload that outlives
//! the token still finishes. A 401 means the request was not processed, so
//! resending it after the renewal is safe even for creating content.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::blocking::{Client as Http, RequestBuilder, Response};
use serde::Deserialize;
use serde_json::Value;

use crate::auth::{is_fresh, now_unix, truncate};
use crate::config::Tokens;
use crate::error::{Error, Result, redact};

const MAX_ATTEMPTS: u32 = 3;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Where a signed-in client gets access tokens.
pub trait Credentials {
    /// Tokens valid for at least the safety margin, refreshed when needed.
    /// Gives up once `deadline` passes.
    fn current(&self, deadline: Option<Instant>) -> Result<Tokens>;
    /// Tokens other than `rejected`, which looked valid but the API refused.
    /// Gives up once `deadline` passes.
    fn renew(&self, rejected: &Tokens, deadline: Option<Instant>) -> Result<Tokens>;
}

/// How a request may be repeated.
#[derive(Debug, Clone, Copy)]
struct Policy {
    /// Retry 429, 5xx gateway errors and timeouts: only safe when idempotent.
    retry_transient: bool,
    /// Renew the token and resend once on 401: only for requests carrying it.
    renew_on_401: bool,
}

const IDEMPOTENT: Policy = Policy {
    retry_transient: true,
    renew_on_401: true,
};
const ONCE: Policy = Policy {
    retry_transient: false,
    renew_on_401: true,
};
/// A pre-signed URL: the URL is the credential, so a 401 is final.
const PRESIGNED: Policy = Policy {
    retry_transient: true,
    renew_on_401: false,
};

pub fn http_client() -> Http {
    Http::builder()
        .user_agent(concat!("yoto-cli/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .build()
        .expect("HTTP client builds")
}

pub struct Client {
    http: Http,
    base: String,
    tokens: RefCell<Tokens>,
    /// `None` for a fixed token that is never renewed.
    credentials: Option<Box<dyn Credentials>>,
    /// While set, renewing the sign-in gives up at this instant: a poll
    /// bounded by a deadline must not be held past it by the renewal.
    deadline: Cell<Option<Instant>>,
    /// Overridable so tests do not sleep.
    pub poll_interval: Duration,
    pub retry_base: Duration,
}

/// What Yoto reports about a finished transcode.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcoded {
    pub sha256: String,
    pub duration: Option<f64>,
    pub file_size: Option<u64>,
    pub channels: Option<Value>,
    pub format: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadSlot {
    pub upload_id: String,
    /// `None` when Yoto already has a file with this SHA-256.
    pub upload_url: Option<String>,
}

impl Client {
    /// A client with a fixed access token that is never renewed.
    pub fn new(http: Http, base: &str, token: String) -> Self {
        let tokens = Tokens {
            client_id: String::new(),
            access_token: token,
            refresh_token: None,
            expires_at: None,
            scope: None,
        };
        Client::build(http, base, tokens, None)
    }

    /// A client that takes its tokens from `credentials` and renews them as
    /// they run out.
    pub fn signed_in(http: Http, base: &str, credentials: Box<dyn Credentials>) -> Result<Self> {
        let tokens = credentials.current(None)?;
        Ok(Client::build(http, base, tokens, Some(credentials)))
    }

    fn build(
        http: Http,
        base: &str,
        tokens: Tokens,
        credentials: Option<Box<dyn Credentials>>,
    ) -> Self {
        Client {
            http,
            base: base.trim_end_matches('/').to_string(),
            tokens: RefCell::new(tokens),
            credentials,
            deadline: Cell::new(None),
            poll_interval: Duration::from_secs(2),
            retry_base: Duration::from_secs(1),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Attach the bearer token, renewing it first when it is about to expire.
    fn authed(&self, rb: RequestBuilder) -> Result<RequestBuilder> {
        if let Some(credentials) = &self.credentials
            && !is_fresh(&self.tokens.borrow(), now_unix())
        {
            let fresh = credentials.current(self.deadline.get())?;
            *self.tokens.borrow_mut() = fresh;
        }
        Ok(rb
            .bearer_auth(&self.tokens.borrow().access_token)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(REQUEST_TIMEOUT))
    }

    /// Send a request under `policy`. The outer error is ours (building the
    /// request, renewing the token); the inner one is the transport's, left
    /// to the caller so a poll can tell a timeout from a hard failure.
    fn exchange(
        &self,
        build: impl Fn() -> Result<RequestBuilder>,
        policy: Policy,
    ) -> Result<reqwest::Result<Response>> {
        let mut attempt = 1;
        let mut renewed = false;
        loop {
            let result = build()?.send();
            if let (Ok(resp), Some(credentials)) = (&result, &self.credentials)
                && resp.status() == StatusCode::UNAUTHORIZED
                && policy.renew_on_401
                && !renewed
            {
                let fresh = credentials.renew(&self.tokens.borrow(), self.deadline.get())?;
                *self.tokens.borrow_mut() = fresh;
                renewed = true;
                continue;
            }
            let retry_after = match &result {
                Ok(resp) if is_transient(resp.status()) => {
                    Some(retry_delay(resp, self.retry_base, attempt))
                }
                Ok(_) => None,
                Err(e) if e.is_timeout() || e.is_connect() => {
                    Some(self.retry_base * 2u32.pow(attempt - 1))
                }
                Err(_) => None,
            };
            match retry_after {
                Some(wait) if policy.retry_transient && attempt < MAX_ATTEMPTS => {
                    std::thread::sleep(wait);
                    attempt += 1;
                }
                _ => return Ok(result),
            }
        }
    }

    /// Send an idempotent request, retrying transient failures.
    fn send_idempotent(&self, build: impl Fn() -> Result<RequestBuilder>) -> Result<Response> {
        Ok(self.exchange(build, IDEMPOTENT)??)
    }

    /// Send a request that must not be repeated once the server may have acted on it.
    fn send_once(&self, build: impl Fn() -> Result<RequestBuilder>) -> Result<Response> {
        Ok(self.exchange(build, ONCE)??)
    }

    fn json(&self, resp: Response, what: &str) -> Result<Value> {
        let status = resp.status();
        let text = resp.text()?;
        json_body(status, &text, what)
    }

    /// `GET /content/mine`: the signed-in user's MYO playlists.
    pub fn list_mine(&self) -> Result<Vec<Value>> {
        let resp =
            self.send_idempotent(|| self.authed(self.http.get(self.url("/content/mine"))))?;
        let body = self.json(resp, "listing playlists")?;
        match body.get("cards") {
            Some(Value::Array(cards)) => Ok(cards.clone()),
            _ => Err(Error::Api {
                status: 200,
                message: "listing playlists: response has no `cards` array".into(),
            }),
        }
    }

    /// `GET /content/{cardId}`.
    pub fn get_content(&self, card_id: &str) -> Result<Value> {
        let path = format!("/content/{}", encode_segment(card_id));
        let resp = self.send_idempotent(|| self.authed(self.http.get(self.url(&path))))?;
        let body = self.json(resp, &format!("playlist {card_id}"))?;
        body.get("card").cloned().ok_or_else(|| Error::Api {
            status: 200,
            message: format!("playlist {card_id}: response has no `card` object"),
        })
    }

    /// `DELETE /content/{cardId}`.
    pub fn delete_content(&self, card_id: &str) -> Result<()> {
        let path = format!("/content/{}", encode_segment(card_id));
        let resp = self.send_idempotent(|| self.authed(self.http.delete(self.url(&path))))?;
        self.json(resp, &format!("playlist {card_id}")).map(|_| ())
    }

    /// `POST /content`: create a playlist, or replace one when the body carries
    /// its `cardId`. Returns the stored `card`.
    pub fn create_content(&self, body: &Value) -> Result<Value> {
        let resp = self.send_once(|| {
            Ok(self
                .authed(self.http.post(self.url("/content")))?
                .json(body))
        })?;
        let body = self.json(resp, "creating the playlist")?;
        body.get("card").cloned().ok_or_else(|| Error::Api {
            status: 200,
            message: "creating the playlist: response has no `card` object".into(),
        })
    }

    /// `GET /media/transcode/audio/uploadUrl?sha256=..&filename=..`.
    pub fn upload_slot(&self, sha256: &str, filename: &str) -> Result<UploadSlot> {
        #[derive(Deserialize)]
        struct Body {
            upload: Slot,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Slot {
            upload_id: String,
            upload_url: Option<String>,
        }
        let resp = self.send_idempotent(|| {
            Ok(self
                .authed(self.http.get(self.url("/media/transcode/audio/uploadUrl")))?
                .query(&[("sha256", sha256), ("filename", filename)]))
        })?;
        let value = self.json(resp, "requesting an upload URL")?;
        let body: Body = serde_json::from_value(value).map_err(|e| Error::Api {
            status: 200,
            message: format!("requesting an upload URL: unexpected response ({e})"),
        })?;
        Ok(UploadSlot {
            upload_id: body.upload.upload_id,
            upload_url: body.upload.upload_url.filter(|u| !u.is_empty()),
        })
    }

    /// PUT the file to the signed upload URL (no bearer token: the URL is the credential).
    pub fn put_audio(&self, upload_url: &str, path: &Path, content_type: &str) -> Result<()> {
        let resp = self.exchange(
            || {
                let file = std::fs::File::open(path)
                    .map_err(|e| Error::invalid(format!("cannot open {}: {e}", path.display())))?;
                Ok(self
                    .http
                    .put(upload_url)
                    .header(reqwest::header::CONTENT_TYPE, content_type)
                    .timeout(Duration::from_secs(30 * 60))
                    .body(file))
            },
            PRESIGNED,
        )??;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        // The storage service's error body can echo the signature
        // (SignatureProvided, StringToSign), so only its code and message
        // are reported.
        let text = resp.text().unwrap_or_default();
        let detail = match (xml_field(&text, "Code"), xml_field(&text, "Message")) {
            (Some(code), Some(message)) => format!("{message} ({code})"),
            (Some(code), None) => code.to_string(),
            (None, _) => format!("HTTP {}", status.as_u16()),
        };
        Err(Error::Api {
            status: status.as_u16(),
            message: format!("uploading audio: {detail}"),
        })
    }

    /// Poll `GET /media/upload/{uploadId}/transcoded` until Yoto reports the
    /// transcoded SHA-256, for at most `timeout`. A 404 counts as "not ready
    /// yet", as the upload record can lag the PUT; so do a transient error
    /// and a request that timed out, since the next poll is the retry. Each
    /// request and each wait is cut to the time left, so the deadline holds
    /// however slow the API is.
    pub fn wait_transcoded(&self, upload_id: &str, timeout: Duration) -> Result<Transcoded> {
        let deadline = Instant::now() + timeout;
        self.deadline.set(Some(deadline));
        let result = self.poll_transcoded(upload_id, timeout, deadline);
        self.deadline.set(None);
        result
    }

    fn poll_transcoded(
        &self,
        upload_id: &str,
        timeout: Duration,
        deadline: Instant,
    ) -> Result<Transcoded> {
        let path = format!("/media/upload/{}/transcoded", encode_segment(upload_id));
        let mut last_failure: Option<String> = None;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                let detail = last_failure
                    .map(|f| format!(" (last check: {f})"))
                    .unwrap_or_default();
                return Err(Error::TranscodeTimeout {
                    message: format!(
                        "upload {upload_id} was not transcoded within {} seconds{detail}",
                        timeout.as_secs()
                    ),
                });
            }
            // The budget is taken when the request is built, so a token
            // renewal before it (or before a retry after a 401) is paid for
            // out of the same deadline. A renewal that ran out of time is a
            // check that did not answer; any other renewal failure is final.
            let result = match self.exchange(
                || {
                    let rb = self
                        .authed(self.http.get(self.url(&path)))?
                        .query(&[("loudnorm", "false")]);
                    let left = deadline.saturating_duration_since(Instant::now());
                    Ok(rb.timeout(left.max(Duration::from_millis(1)).min(REQUEST_TIMEOUT)))
                },
                ONCE,
            ) {
                Ok(result) => result,
                Err(e @ (Error::Network { .. } | Error::TokenStore { .. }))
                    if Instant::now() >= deadline =>
                {
                    last_failure = Some(e.to_string());
                    continue;
                }
                Err(e) => return Err(e),
            };
            let mut wait = self.poll_interval;
            let transport = |e: reqwest::Error| {
                if e.is_timeout() || e.is_connect() {
                    Ok(redact(e).to_string())
                } else {
                    Err(Error::from(e))
                }
            };
            match result {
                Ok(resp) if resp.status() == StatusCode::NOT_FOUND => last_failure = None,
                Ok(resp) if is_transient(resp.status()) => {
                    wait = wait.max(retry_delay(&resp, self.poll_interval, 1));
                    last_failure = Some(format!("HTTP {}", resp.status().as_u16()));
                }
                Ok(resp) => {
                    let status = resp.status();
                    match resp.text() {
                        Ok(text) => {
                            let body = json_body(status, &text, "checking the transcode")?;
                            if let Some(t) = parse_transcoded(&body)? {
                                return Ok(t);
                            }
                            last_failure = None;
                        }
                        Err(e) => last_failure = Some(transport(e)?),
                    }
                }
                Err(e) => last_failure = Some(transport(e)?),
            }
            std::thread::sleep(wait.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    /// `POST /media/coverImage/user/me/upload`: returns the cover's `mediaUrl`.
    pub fn upload_cover(&self, bytes: Vec<u8>, content_type: &str) -> Result<String> {
        let resp = self.send_once(|| {
            Ok(self
                .authed(self.http.post(self.url("/media/coverImage/user/me/upload")))?
                .query(&[("autoconvert", "true"), ("coverType", "default")])
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(bytes.clone()))
        })?;
        let body = self.json(resp, "uploading the cover")?;
        body.pointer("/coverImage/mediaUrl")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Api {
                status: 200,
                message: "uploading the cover: response has no coverImage.mediaUrl".into(),
            })
    }

    /// `POST /media/displayIcons/user/me/upload`: returns the icon's `mediaId`.
    ///
    /// Yoto stores an identical image only once and answers a re-upload with
    /// the existing record, so this is safe to retry. `auto_convert` lets Yoto
    /// resize the image to 16x16 and tone down its brightness; without it the
    /// image must already be a 16x16 RGBA PNG or a GIF, and is kept as drawn.
    pub fn upload_icon(
        &self,
        bytes: &[u8],
        filename: &str,
        content_type: &str,
        auto_convert: bool,
    ) -> Result<String> {
        // The endpoint takes the image itself as the request body; a multipart
        // form is rejected with "A binary image file is required".
        let resp = self.send_idempotent(|| {
            Ok(self
                .authed(
                    self.http
                        .post(self.url("/media/displayIcons/user/me/upload")),
                )?
                .query(&[
                    ("autoConvert", if auto_convert { "true" } else { "false" }),
                    ("filename", filename),
                ])
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(bytes.to_vec()))
        })?;
        let body = self.json(resp, &format!("uploading icon {filename}"))?;
        body.pointer("/displayIcon/mediaId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| Error::Api {
                status: 200,
                message: format!("uploading icon {filename}: response has no displayIcon.mediaId"),
            })
    }

    /// `GET /media/displayIcons/user/yoto` (public icons) or `/user/me` (your uploads).
    pub fn display_icons(&self, public: bool) -> Result<Vec<Value>> {
        let path = if public {
            "/media/displayIcons/user/yoto"
        } else {
            "/media/displayIcons/user/me"
        };
        let resp = self.send_idempotent(|| self.authed(self.http.get(self.url(path))))?;
        let body = self.json(resp, "listing icons")?;
        match body.get("displayIcons") {
            Some(Value::Array(icons)) => Ok(icons.clone()),
            _ => Err(Error::Api {
                status: 200,
                message: "listing icons: response has no `displayIcons` array".into(),
            }),
        }
    }
}

/// The finished transcode, `None` while Yoto is still working on it, or an
/// error once Yoto reports that it gave up on the file.
fn parse_transcoded(body: &Value) -> Result<Option<Transcoded>> {
    let Some(t) = body.get("transcode") else {
        return Ok(None);
    };
    let sha256 = t
        .get("transcodedSha256")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(sha256) = sha256 else {
        return match transcode_failure(t) {
            Some(message) => Err(Error::TranscodeFailed { message }),
            None => Ok(None),
        };
    };
    let info = t.get("transcodedInfo");
    let field = |name: &str| info.and_then(|i| i.get(name));
    Ok(Some(Transcoded {
        sha256: sha256.to_string(),
        duration: field("duration").and_then(Value::as_f64),
        file_size: field("fileSize").and_then(Value::as_u64),
        channels: field("channels").filter(|v| !v.is_null()).cloned(),
        format: field("format").and_then(Value::as_str).map(str::to_string),
    }))
}

/// Why Yoto gave up on a transcode, when its `error` or `erroredAt` says it did.
fn transcode_failure(t: &Value) -> Option<String> {
    let error = t.get("error").filter(|e| !e.is_null());
    let errored = t.get("erroredAt").is_some_and(|e| !e.is_null());
    if error.is_none() && !errored {
        return None;
    }
    let reason = error
        .and_then(|e| e.get("message").and_then(Value::as_str).or(e.as_str()))
        .unwrap_or("no reason given");
    Some(match t.get("uploadFilename").and_then(Value::as_str) {
        Some(name) => format!("Yoto could not transcode {name}: {reason}"),
        None => format!("Yoto could not transcode the upload: {reason}"),
    })
}

/// The text of the first `<name>` element, for the flat error XML that
/// object storage returns.
fn xml_field<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let len = xml[start..].find(&format!("</{name}>"))?;
    Some(xml[start..start + len].trim()).filter(|s| !s.is_empty())
}

fn json_body(status: StatusCode, text: &str, what: &str) -> Result<Value> {
    if status.is_success() {
        return serde_json::from_str(text).map_err(|e| Error::Api {
            status: status.as_u16(),
            message: format!(
                "{what}: response is not JSON ({e}): {}",
                truncate(text, 200)
            ),
        });
    }
    Err(status_error(status, text, what))
}

fn is_transient(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::BAD_GATEWAY
        || status == StatusCode::SERVICE_UNAVAILABLE
        || status == StatusCode::GATEWAY_TIMEOUT
}

fn retry_delay(resp: &Response, base: Duration, attempt: u32) -> Duration {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(base * 2u32.pow(attempt - 1))
        .min(MAX_RETRY_AFTER)
}

/// Map an error status to an error, using Yoto's `{"error":{"code","message"}}` body when present.
fn status_error(status: StatusCode, text: &str, what: &str) -> Error {
    let detail = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| {
            let err = v.get("error")?;
            let message = err.get("message").and_then(Value::as_str);
            let code = err.get("code").and_then(Value::as_str);
            match (code, message) {
                (Some(c), Some(m)) => Some(format!("{m} ({c})")),
                (None, Some(m)) => Some(m.to_string()),
                (Some(c), None) => Some(c.to_string()),
                (None, None) => err.as_str().map(str::to_string),
            }
        })
        .unwrap_or_else(|| truncate(text, 200));
    let message = format!("{what}: {detail}");
    match status {
        StatusCode::UNAUTHORIZED => Error::NotAuthenticated { message },
        StatusCode::NOT_FOUND => Error::NotFound { message },
        _ => Error::Api {
            status: status.as_u16(),
            message,
        },
    }
}

fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transcoded_requires_a_sha() {
        assert_eq!(parse_transcoded(&json!({"transcode": {}})).unwrap(), None);
        assert_eq!(
            parse_transcoded(&json!({"transcode": {"transcodedSha256": ""}})).unwrap(),
            None
        );
        // A transcode still in progress carries its failure fields as null.
        assert_eq!(
            parse_transcoded(&json!({"transcode": {
                "transcodedSha256": null, "error": null, "erroredAt": null
            }}))
            .unwrap(),
            None
        );
        let t = parse_transcoded(&json!({"transcode": {
            "transcodedSha256": "abc",
            "transcodedInfo": {"duration": 12.5, "fileSize": 100, "channels": "stereo", "format": "mp3"}
        }}))
        .unwrap()
        .unwrap();
        assert_eq!(t.sha256, "abc");
        assert_eq!(t.duration, Some(12.5));
        assert_eq!(t.file_size, Some(100));
        assert_eq!(t.channels, Some(json!("stereo")));
        assert_eq!(t.format.as_deref(), Some("mp3"));
    }

    #[test]
    fn a_failure_is_reported_from_either_field_with_a_fallback_reason() {
        let only_errored_at = parse_transcoded(&json!({"transcode": {
            "transcodedSha256": null, "error": null, "erroredAt": "2026-10-26T08:31:34.813Z"
        }}))
        .unwrap_err();
        assert_eq!(only_errored_at.kind(), "transcode_failed");
        assert_eq!(
            only_errored_at.to_string(),
            "Yoto could not transcode the upload: no reason given"
        );
        let only_error = parse_transcoded(&json!({"transcode": {
            "uploadFilename": "a.wav", "error": {"message": "bad header"}
        }}))
        .unwrap_err();
        assert_eq!(
            only_error.to_string(),
            "Yoto could not transcode a.wav: bad header"
        );
    }

    #[test]
    fn status_errors_use_the_yoto_envelope() {
        let e = status_error(
            StatusCode::NOT_FOUND,
            r#"{"error":{"code":"not-found","message":"Card abc123 does not exist."}}"#,
            "playlist abc123",
        );
        assert_eq!(e.kind(), "not_found");
        assert_eq!(
            e.to_string(),
            "playlist abc123: Card abc123 does not exist. (not-found)"
        );
        let e = status_error(StatusCode::BAD_REQUEST, "plain text", "x");
        assert_eq!(
            e,
            Error::Api {
                status: 400,
                message: "x: plain text".into()
            }
        );
    }

    /// Hands out `old` (about to expire) first and `new` on every later call.
    struct Expiring {
        calls: std::cell::Cell<u32>,
    }

    impl Credentials for Expiring {
        fn current(&self, _: Option<Instant>) -> Result<Tokens> {
            let n = self.calls.get();
            self.calls.set(n + 1);
            let (token, expires_at) = if n == 0 {
                ("old", now_unix() + 5)
            } else {
                ("new", now_unix() + 3600)
            };
            Ok(Tokens {
                client_id: "c".into(),
                access_token: token.into(),
                refresh_token: Some("r".into()),
                expires_at: Some(expires_at),
                scope: None,
            })
        }

        fn renew(&self, _: &Tokens, _: Option<Instant>) -> Result<Tokens> {
            panic!("an expiring token must be renewed before it is sent, not after a 401")
        }
    }

    #[test]
    fn a_token_about_to_expire_is_renewed_before_the_request() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/content/mine")
            .match_header("authorization", "Bearer new")
            .with_body(r#"{"cards":[]}"#)
            .expect(1)
            .create();
        let credentials = Box::new(Expiring {
            calls: std::cell::Cell::new(0),
        });
        let client = Client::signed_in(http_client(), &server.url(), credentials).unwrap();
        assert_eq!(client.list_mine().unwrap(), Vec::<Value>::new());
        mock.assert();
    }

    fn fast_client(base: &str) -> Client {
        let mut client = Client::new(http_client(), base, "t".into());
        client.poll_interval = Duration::from_millis(50);
        client.retry_base = Duration::from_millis(10);
        client
    }

    #[test]
    fn a_transcode_yoto_reports_as_failed_ends_the_wait_with_its_reason() {
        let mut server = mockito::Server::new();
        // The shape Yoto returns for a file its transcoder cannot read.
        let failed = server
            .mock("GET", "/media/upload/u1/transcoded")
            .match_query(mockito::Matcher::Any)
            .with_status(202)
            .with_body(
                json!({"transcode": {
                    "uploadId": "u1",
                    "uploadFilename": "05 - AIFF.aiff",
                    "error": {"name": "TranscodeError", "message": "file is not a valid audio/video file"},
                    "erroredAt": "2026-10-26T08:31:34.813Z",
                    "transcodedAt": null,
                    "transcodedSha256": null
                }})
                .to_string(),
            )
            .expect(1)
            .create();
        let started = Instant::now();
        let err = fast_client(&server.url())
            .wait_transcoded("u1", Duration::from_secs(5))
            .unwrap_err();
        assert_eq!(err.kind(), "transcode_failed", "{err}");
        assert!(err.to_string().contains("05 - AIFF.aiff"), "{err}");
        assert!(err.to_string().contains("not a valid audio"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
        failed.assert();
    }

    #[test]
    fn a_server_that_never_answers_cannot_hold_the_transcode_past_its_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                held.push(stream);
            }
        });
        let started = Instant::now();
        let err = fast_client(&base)
            .wait_transcoded("u1", Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(err.kind(), "transcode_timeout", "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_failed_signed_upload_does_not_print_its_signature() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.mp3");
        std::fs::write(&file, b"x").unwrap();
        // Nothing listens on a port just released by a listener.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}/audio/a?X-Amz-Signature=sekrit");
        let err = fast_client("http://unused")
            .put_audio(&url, &file, "audio/mpeg")
            .unwrap_err();
        assert_eq!(err.kind(), "network", "{err}");
        assert!(!err.to_string().contains("sekrit"), "{err}");
    }

    #[test]
    fn a_rejected_signed_upload_reports_the_storage_code_not_the_signature() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.mp3");
        std::fs::write(&file, b"x").unwrap();
        let mut server = mockito::Server::new();
        server
            .mock("PUT", "/audio/a")
            .match_query(mockito::Matcher::Any)
            .with_status(403)
            .with_body(concat!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Error><Code>SignatureDoesNotMatch</Code>"#,
                "<Message>The request signature we calculated does not match.</Message>",
                "<SignatureProvided>sekrit</SignatureProvided><StringToSign>sekrit</StringToSign></Error>",
            ))
            .create();
        let url = format!("{}/audio/a?X-Amz-Signature=sekrit", server.url());
        let err = fast_client("http://unused")
            .put_audio(&url, &file, "audio/mpeg")
            .unwrap_err();
        let text = err.to_string();
        assert!(!text.contains("sekrit"), "{text}");
        assert!(text.contains("SignatureDoesNotMatch"), "{text}");
        assert!(text.contains("403"), "{text}");
    }

    /// Accepts connections and answers each with headers and part of a body,
    /// then stalls.
    fn stalled_body_server() -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{",
                );
                held.push(stream);
            }
        });
        base
    }

    #[test]
    fn a_stalled_response_body_is_a_poll_that_did_not_answer() {
        let started = Instant::now();
        let err = fast_client(&stalled_body_server())
            .wait_transcoded("u1", Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(err.kind(), "transcode_timeout", "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    /// Hands out `old`, and `new` only after a slow renewal.
    struct SlowRenewal;

    impl Credentials for SlowRenewal {
        fn current(&self, _: Option<Instant>) -> Result<Tokens> {
            Ok(Tokens {
                client_id: "c".into(),
                access_token: "old".into(),
                refresh_token: Some("r".into()),
                expires_at: Some(now_unix() + 3600),
                scope: None,
            })
        }

        fn renew(&self, _: &Tokens, _: Option<Instant>) -> Result<Tokens> {
            std::thread::sleep(Duration::from_millis(900));
            Ok(Tokens {
                access_token: "new".into(),
                ..self.current(None)?
            })
        }
    }

    #[test]
    fn a_renewal_during_a_poll_counts_against_the_transcode_deadline() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/media/upload/u1/transcoded")
            .match_query(mockito::Matcher::Any)
            .match_header("authorization", "Bearer old")
            .with_status(401)
            .create();
        server
            .mock("GET", "/media/upload/u1/transcoded")
            .match_query(mockito::Matcher::Any)
            .match_header("authorization", "Bearer new")
            .with_body_from_request(|_| {
                std::thread::sleep(Duration::from_secs(2));
                r#"{"transcode":{}}"#.into()
            })
            .create();
        let mut client =
            Client::signed_in(http_client(), &server.url(), Box::new(SlowRenewal)).unwrap();
        client.poll_interval = Duration::from_millis(50);
        let started = Instant::now();
        let err = client
            .wait_transcoded("u1", Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(err.kind(), "transcode_timeout", "{err}");
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_long_retry_after_cannot_hold_the_transcode_past_its_deadline() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/media/upload/u1/transcoded")
            .match_query(mockito::Matcher::Any)
            .with_status(503)
            .with_header("retry-after", "30")
            .create();
        let started = Instant::now();
        let err = fast_client(&server.url())
            .wait_transcoded("u1", Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(err.kind(), "transcode_timeout", "{err}");
        assert!(err.to_string().contains("HTTP 503"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(encode_segment("aB3-_"), "aB3-_");
        assert_eq!(encode_segment("../x y"), "..%2Fx%20y");
    }
}
