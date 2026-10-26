//! Error type, the stable error `kind` set, and the exit-code contract.
//!
//! Errors are reported as a clispec structured envelope on the last line of
//! stderr: `{"error":{"kind":...,"message":...,"exit_code":...,"hint":...}}`.
//! Every kind returned by `kind()` is declared in `schema.rs`; a test keeps the
//! two in sync.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    /// Invalid command-line arguments (also wraps clap errors).
    #[error("{message}")]
    Usage { message: String },

    /// A local input (directory, manifest, audio file, cover) is missing or unusable.
    #[error("{message}")]
    InvalidInput { message: String },

    /// The playlist would break a Yoto limit (tracks, track size or length, playlist size).
    #[error("{message}")]
    LimitExceeded { message: String },

    /// No usable sign-in: never logged in, or the refresh token was rejected.
    #[error("{message}")]
    NotAuthenticated { message: String },

    /// The browser sign-in did not complete (denied, wrong state, timed out).
    #[error("{message}")]
    AuthFailed { message: String },

    /// A mutating command was run without `--yes`.
    #[error("{message}")]
    ConfirmationRequired { message: String },

    /// The requested content does not exist or is not yours.
    #[error("{message}")]
    NotFound { message: String },

    /// The Yoto API answered with an error status.
    #[error("Yoto API returned {status}: {message}")]
    Api { status: u16, message: String },

    /// The request never got an HTTP answer (DNS, TLS, connection, timeout).
    #[error("{message}")]
    Network { message: String },

    /// Yoto did not finish transcoding an upload within the wait limit.
    #[error("{message}")]
    TranscodeTimeout { message: String },

    /// Reading or writing the stored sign-in (keychain or token file) failed.
    #[error("{message}")]
    TokenStore { message: String },

    /// The playlist changed between reading it and saving the edit.
    #[error("{message}")]
    Conflict { message: String },
}

impl Error {
    /// Stable snake_case identifier consumers branch on (the schema `errors` set).
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Usage { .. } => "usage",
            Error::InvalidInput { .. } => "invalid_input",
            Error::LimitExceeded { .. } => "limit_exceeded",
            Error::NotAuthenticated { .. } => "not_authenticated",
            Error::AuthFailed { .. } => "auth_failed",
            Error::ConfirmationRequired { .. } => "confirmation_required",
            Error::NotFound { .. } => "not_found",
            Error::Api { .. } => "api_error",
            Error::Network { .. } => "network",
            Error::TranscodeTimeout { .. } => "transcode_timeout",
            Error::TokenStore { .. } => "token_store",
            Error::Conflict { .. } => "conflict",
        }
    }

    /// Actionable remediation, when there is one.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Error::Usage { .. } => Some("see `yoto-cli --help` or `yoto-cli schema`"),
            Error::InvalidInput { .. } => None,
            Error::LimitExceeded { .. } => Some(
                "split the audio over more playlists: max 100 tracks, 100 MB and 60 minutes per track, 500 MB per playlist",
            ),
            Error::NotAuthenticated { .. } => Some("run `yoto-cli auth login`"),
            Error::AuthFailed { .. } => Some("run `yoto-cli auth login` again"),
            Error::ConfirmationRequired { .. } => {
                Some("review with --dry-run, then re-run with --yes")
            }
            Error::NotFound { .. } => Some("list your playlists with `yoto-cli content list`"),
            Error::Api { .. } => None,
            Error::Network { .. } => Some("check the connection and retry"),
            Error::TranscodeTimeout { .. } => {
                Some("retry the upload; files Yoto already received are not uploaded again")
            }
            Error::TokenStore { .. } => Some(
                "on a host without a usable keychain (no Secret Service on Linux), set YOTO_CLI_TOKEN_FILE to keep the sign-in in a 0600 file",
            ),
            Error::Conflict { .. } => Some(
                "re-run the command to apply it to the current playlist; audio already uploaded is not uploaded again",
            ),
        }
    }

    /// The process exit code associated with this error.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::InvalidInput { .. } => 1,
            Error::Usage { .. } => 3,
            Error::NotAuthenticated { .. } => 4,
            Error::ConfirmationRequired { .. } => 5,
            Error::LimitExceeded { .. } => 6,
            Error::NotFound { .. } => 7,
            Error::Api { .. } => 8,
            Error::Network { .. } => 9,
            Error::TranscodeTimeout { .. } => 10,
            Error::AuthFailed { .. } => 11,
            Error::TokenStore { .. } => 12,
            Error::Conflict { .. } => 13,
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Error::InvalidInput {
            message: message.into(),
        }
    }

    pub(crate) fn usage(message: impl Into<String>) -> Self {
        Error::Usage {
            message: message.into(),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Network {
            message: format!("request failed: {}", redact(e)),
        }
    }
}

/// Drop the query string from the URL a transport error names: on a signed
/// upload URL it carries the signature, which must not reach stderr or logs.
pub(crate) fn redact(mut e: reqwest::Error) -> reqwest::Error {
    if let Some(url) = e.url_mut() {
        url.set_query(None);
    }
    e
}

pub type Result<T> = std::result::Result<T, Error>;
