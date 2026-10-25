//! Where yoto-cli talks to and where it keeps its state.
//!
//! The client ID lives in a plain config file (it is public by design: Yoto
//! only supports public PKCE clients). Tokens live in the OS keychain, or in a
//! 0600 file when `YOTO_CLI_TOKEN_FILE` is set (headless hosts, tests).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::keychain;

pub const DEFAULT_API_URL: &str = "https://api.yotoplay.com";
pub const DEFAULT_AUTH_URL: &str = "https://login.yotoplay.com";
/// The OAuth audience Yoto issues API tokens for.
pub const AUDIENCE: &str = "https://api.yotoplay.com";

/// Base URLs, overridable so tests can point at a local mock server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub api: String,
    pub auth: String,
}

impl Endpoints {
    pub fn from_env() -> Self {
        Endpoints {
            api: env_or("YOTO_CLI_API_URL", DEFAULT_API_URL),
            auth: env_or("YOTO_CLI_AUTH_URL", DEFAULT_AUTH_URL),
        }
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| v.trim_end_matches('/').to_string())
        .unwrap_or_else(|| default.to_string())
}

/// The directory holding `config.json`.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("YOTO_CLI_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::config_dir()
        .map(|d| d.join("yoto-cli"))
        .ok_or_else(|| Error::TokenStore {
            message: "cannot determine the user config directory; set YOTO_CLI_CONFIG_DIR".into(),
        })
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

impl Config {
    pub fn load(dir: &Path) -> Result<Config> {
        let path = dir.join("config.json");
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| Error::TokenStore {
                message: format!("{} is not valid config JSON: {e}", path.display()),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(Error::TokenStore {
                message: format!("cannot read {}: {e}", path.display()),
            }),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self).expect("config serializes");
        write_private(&dir.join("config.json"), text.as_bytes())
    }
}

/// The OAuth tokens for one signed-in account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tokens {
    pub client_id: String,
    pub access_token: String,
    /// Single-use: every refresh returns a new one that replaces it.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix seconds. `None` when the server gave no expiry, which is treated as expired.
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// Where tokens are persisted.
#[derive(Debug, Clone)]
pub enum TokenStore {
    Keychain,
    File(PathBuf),
}

impl TokenStore {
    pub fn from_env() -> Self {
        match std::env::var_os("YOTO_CLI_TOKEN_FILE").filter(|v| !v.is_empty()) {
            Some(path) => TokenStore::File(PathBuf::from(path)),
            None => TokenStore::Keychain,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            TokenStore::Keychain => "keychain".into(),
            TokenStore::File(p) => format!("file {}", p.display()),
        }
    }

    pub fn load(&self) -> Result<Option<Tokens>> {
        let text = match self {
            TokenStore::Keychain => match keychain::load(&keychain::Keyring::default())? {
                Some(t) => t,
                None => return Ok(None),
            },
            TokenStore::File(path) => match fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(store_err(&format!("read {}", path.display()), e)),
            },
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| store_err("parse the stored sign-in", e))
    }

    pub fn save(&self, tokens: &Tokens) -> Result<()> {
        let text = serde_json::to_string(tokens).expect("tokens serialize");
        match self {
            TokenStore::Keychain => {
                keychain::save(&keychain::Keyring::default(), &text, keychain::ENTRY_LIMIT)
            }
            TokenStore::File(path) => write_private(path, text.as_bytes()),
        }
    }

    /// Hold an exclusive lock shared by every yoto-cli process using this
    /// store, until the returned file is dropped. Refreshing spends the
    /// single-use refresh token, so only one process may do it at a time.
    pub fn lock(&self) -> Result<fs::File> {
        self.lock_until(None)
    }

    /// [`lock`](Self::lock), giving up once `deadline` passes.
    pub fn lock_until(&self, deadline: Option<Instant>) -> Result<fs::File> {
        let path = match self {
            TokenStore::Keychain => config_dir()?.join("tokens.lock"),
            TokenStore::File(path) => {
                let mut name = path.as_os_str().to_owned();
                name.push(".lock");
                PathBuf::from(name)
            }
        };
        let err = |e: std::io::Error| store_err(&format!("lock {}", path.display()), e);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(err)?;
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(err)?;
        let Some(deadline) = deadline else {
            file.lock().map_err(err)?;
            return Ok(file);
        };
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(fs::TryLockError::WouldBlock) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(Error::TokenStore {
                            message: format!(
                                "timed out waiting for {}: another yoto-cli process is renewing the sign-in",
                                path.display()
                            ),
                        });
                    }
                    std::thread::sleep(left.min(Duration::from_millis(25)));
                }
                Err(fs::TryLockError::Error(e)) => return Err(err(e)),
            }
        }
    }

    /// Remove the stored sign-in. Returns whether there was one.
    pub fn delete(&self) -> Result<bool> {
        match self {
            TokenStore::Keychain => keychain::delete(&keychain::Keyring::default()),
            TokenStore::File(path) => match fs::remove_file(path) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(store_err(&format!("delete {}", path.display()), e)),
            },
        }
    }
}

fn store_err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::TokenStore {
        message: format!("cannot {what}: {e}"),
    }
}

/// Write a file readable only by the owner, replacing it atomically.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let err = |e: std::io::Error| store_err(&format!("write {}", path.display()), e);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(err)?;
    }
    // A fresh, exclusively created, owner-only temporary file: nothing already
    // beside the target (a readable file, a symlink) can receive the bytes.
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(err)?;
    tmp.write_all(bytes).map_err(err)?;
    tmp.as_file().sync_all().map_err(err)?;
    tmp.persist(path).map_err(|e| err(e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trips_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::File(dir.path().join("sub/tokens.json"));
        assert_eq!(store.load().unwrap(), None);
        let tokens = Tokens {
            client_id: "cid".into(),
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: Some(10),
            scope: None,
        };
        store.save(&tokens).unwrap();
        assert_eq!(store.load().unwrap(), Some(tokens));
        assert!(store.delete().unwrap());
        assert!(!store.delete().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        write_private(&path, b"{}").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn missing_config_is_default_and_saves() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(dir.path()).unwrap(), Config::default());
        let cfg = Config {
            client_id: Some("abc".into()),
        };
        cfg.save(dir.path()).unwrap();
        assert_eq!(Config::load(dir.path()).unwrap(), cfg);
    }
}
