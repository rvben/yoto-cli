//! The stored sign-in in the OS keychain, split over several entries where
//! one entry is too small to hold it.
//!
//! Windows Credential Manager caps a secret at 2560 bytes of UTF-16, which a
//! JWT access token plus a refresh token can exceed. A document that fits is
//! stored as is in the `tokens` entry. One that does not is written as parts
//! `tokens.<generation>.<n>`, and `tokens` then holds only a header naming
//! them. The parts are written before the header and the previous
//! generation's parts are removed after it, so an interrupted save leaves
//! the previous sign-in complete and readable. The parts such a save leaves
//! behind sit in the one generation the next save writes to, which every
//! save and every logout sweeps, so no tokens outlive a logout.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const SERVICE: &str = "yoto-cli";
const ACCOUNT: &str = "tokens";

/// The most UTF-16 code units one entry holds. Credential Manager stores the
/// secret as UTF-16 in at most 2560 bytes; elsewhere there is no such cap.
#[cfg(windows)]
pub const ENTRY_LIMIT: usize = 2560 / 2;
#[cfg(not(windows))]
pub const ENTRY_LIMIT: usize = usize::MAX;

/// Named secrets under one service.
pub trait Backend {
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, secret: &str) -> Result<()>;
    /// Returns whether there was an entry to remove.
    fn delete(&self, account: &str) -> Result<bool>;
}

/// The OS keychain: macOS Keychain, Windows Credential Manager or the Linux
/// Secret Service.
pub struct Keyring {
    service: String,
}

impl Default for Keyring {
    fn default() -> Self {
        Keyring::with_service(SERVICE)
    }
}

impl Keyring {
    pub fn with_service(service: &str) -> Self {
        Keyring {
            service: service.to_string(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, account).map_err(|e| err("open the keychain", e))
    }
}

impl Backend for Keyring {
    fn get(&self, account: &str) -> Result<Option<String>> {
        match self.entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(err("read the keychain", e)),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        self.entry(account)?
            .set_password(secret)
            .map_err(|e| err("write the keychain", e))
    }

    fn delete(&self, account: &str) -> Result<bool> {
        match self.entry(account)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(err("delete from the keychain", e)),
        }
    }
}

/// What the `tokens` entry holds when the document was split.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Header {
    generation: u64,
    parts: usize,
}

fn part_account(generation: u64, n: usize) -> String {
    format!("{ACCOUNT}.{generation}.{n}")
}

fn header(stored: &str) -> Option<Header> {
    serde_json::from_str(stored).ok()
}

/// How often [`load`] starts over because another process saved a newer
/// sign-in while it was reading the parts.
const LOAD_ATTEMPTS: usize = 5;

/// The stored document, joined from its parts when it was split.
///
/// Readers take no lock, so a save in another process can remove the parts
/// of the generation being read. A missing part under a header that has
/// since changed means exactly that, and the read starts over.
pub fn load(backend: &dyn Backend) -> Result<Option<String>> {
    let mut stored = backend.get(ACCOUNT)?;
    for _ in 0..LOAD_ATTEMPTS {
        let Some(current) = stored else {
            return Ok(None);
        };
        let Some(h) = header(&current) else {
            return Ok(Some(current));
        };
        match join(backend, &h)? {
            Ok(text) => return Ok(Some(text)),
            Err(missing) => {
                let latest = backend.get(ACCOUNT)?;
                if latest.as_deref() == Some(current.as_str()) {
                    return Err(missing);
                }
                stored = latest;
            }
        }
    }
    Err(err(
        "read the keychain",
        "the stored sign-in kept changing while it was read",
    ))
}

/// The parts named by `h` joined, or the error for the first missing one.
fn join(backend: &dyn Backend, h: &Header) -> Result<std::result::Result<String, Error>> {
    let mut text = String::new();
    for n in 0..h.parts {
        let Some(part) = backend.get(&part_account(h.generation, n))? else {
            return Ok(Err(err(
                "read the keychain",
                format!("part {n} of {} of the stored sign-in is missing", h.parts),
            )));
        };
        text.push_str(&part);
    }
    Ok(Ok(text))
}

/// The generation a save writes its parts to: one past the stored header's,
/// or 1 without one. A save that never got to write its header leaves its
/// parts here, so this is the one place to look for them.
fn next_generation(h: Option<&Header>) -> u64 {
    h.map_or(1, |h| h.generation + 1)
}

/// Store `text`, split into entries of at most `limit` UTF-16 code units.
pub fn save(backend: &dyn Backend, text: &str, limit: usize) -> Result<()> {
    let previous = backend.get(ACCOUNT)?.as_deref().and_then(header);
    let generation = next_generation(previous.as_ref());
    let parts = split(text, limit);
    let written = if parts.len() == 1 {
        backend.set(ACCOUNT, text)?;
        0
    } else {
        let h = Header {
            generation,
            parts: parts.len(),
        };
        let stored = parts
            .iter()
            .enumerate()
            .try_for_each(|(n, part)| backend.set(&part_account(generation, n), part))
            .and_then(|()| {
                backend.set(
                    ACCOUNT,
                    &serde_json::to_string(&h).expect("header serializes"),
                )
            });
        if let Err(e) = stored {
            // Best effort: what is left is swept by the next save or logout.
            let _ = remove_parts(backend, generation, 0, 0);
            return Err(e);
        }
        h.parts
    };
    // Parts beyond the ones just written are left from an interrupted save.
    remove_parts(backend, generation, written, 0)?;
    if let Some(old) = previous {
        remove_parts(backend, old.generation, 0, old.parts)?;
    }
    Ok(())
}

/// Remove the stored document and its parts, including any left by an
/// interrupted save. Returns whether there was a document.
pub fn delete(backend: &dyn Backend) -> Result<bool> {
    let stored = backend.get(ACCOUNT)?;
    let h = stored.as_deref().and_then(header);
    // The header goes first: without it nothing reads the parts, so a
    // delete cut short still leaves the user signed out.
    if stored.is_some() {
        backend.delete(ACCOUNT)?;
    }
    if let Some(h) = &h {
        remove_parts(backend, h.generation, 0, h.parts)?;
    }
    remove_parts(backend, next_generation(h.as_ref()), 0, 0)?;
    Ok(stored.is_some())
}

/// Remove the parts of `generation` from part `from` on: every one below
/// `at_least`, and past that each next one for as long as it exists.
fn remove_parts(
    backend: &dyn Backend,
    generation: u64,
    from: usize,
    at_least: usize,
) -> Result<()> {
    let mut n = from;
    while backend.delete(&part_account(generation, n))? || n < at_least {
        n += 1;
    }
    Ok(())
}

/// Split at char boundaries into pieces of at most `limit` UTF-16 code units.
fn split(text: &str, limit: usize) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut units = 0;
    for (i, c) in text.char_indices() {
        if units + c.len_utf16() > limit {
            parts.push(&text[start..i]);
            start = i;
            units = 0;
        }
        units += c.len_utf16();
    }
    parts.push(&text[start..]);
    parts
}

fn err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::TokenStore {
        message: format!("cannot {what}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// An in-memory keychain that can be told to fail a write.
    #[derive(Default)]
    struct Memory {
        entries: RefCell<BTreeMap<String, String>>,
        fail_set: Option<&'static str>,
    }

    impl Backend for Memory {
        fn get(&self, account: &str) -> Result<Option<String>> {
            Ok(self.entries.borrow().get(account).cloned())
        }

        fn set(&self, account: &str, secret: &str) -> Result<()> {
            if self.fail_set == Some(account) {
                return Err(err("write the keychain", "interrupted"));
            }
            self.entries
                .borrow_mut()
                .insert(account.to_string(), secret.to_string());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<bool> {
            Ok(self.entries.borrow_mut().remove(account).is_some())
        }
    }

    impl Memory {
        fn accounts(&self) -> Vec<String> {
            self.entries.borrow().keys().cloned().collect()
        }
    }

    const LONG: &str = r#"{"access_token":"eyJhbGciOi.payload.sig","refresh_token":"r-123"}"#;

    #[test]
    fn a_document_that_fits_is_one_entry() {
        let m = Memory::default();
        save(&m, LONG, 1000).unwrap();
        assert_eq!(m.accounts(), ["tokens"]);
        assert_eq!(load(&m).unwrap().as_deref(), Some(LONG));
    }

    #[test]
    fn a_document_too_long_for_one_entry_is_split_and_joined() {
        let m = Memory::default();
        save(&m, LONG, 10).unwrap();
        let entries = m.entries.borrow().clone();
        assert!(entries.len() > 2, "{entries:?}");
        for (account, secret) in &entries {
            assert!(secret.encode_utf16().count() <= 10 || account == "tokens");
        }
        assert_eq!(load(&m).unwrap().as_deref(), Some(LONG));
    }

    #[test]
    fn parts_respect_the_limit_in_utf16_units_not_bytes_or_chars() {
        // U+1F600 is 2 UTF-16 units and 4 bytes; é is 1 unit and 2 bytes.
        let text = "é😀é😀é😀";
        let parts = split(text, 3);
        assert!(
            parts.iter().all(|p| p.encode_utf16().count() <= 3),
            "{parts:?}"
        );
        assert_eq!(parts.concat(), text);
    }

    #[test]
    fn a_new_save_removes_the_previous_parts() {
        let m = Memory::default();
        save(&m, LONG, 10).unwrap();
        save(&m, &LONG.replace("r-123", "r-456"), 10).unwrap();
        assert!(
            m.accounts()
                .iter()
                .all(|a| a == "tokens" || a.starts_with("tokens.2."))
        );
        save(&m, "{}", 10).unwrap();
        assert_eq!(m.accounts(), ["tokens"]);
        assert_eq!(load(&m).unwrap().as_deref(), Some("{}"));
    }

    #[test]
    fn an_interrupted_save_keeps_the_previous_sign_in() {
        let mut m = Memory::default();
        save(&m, LONG, 10).unwrap();
        // The write dies partway through the new generation's parts.
        m.fail_set = Some("tokens.2.1");
        let newer = LONG.replace("r-123", "r-456");
        assert!(save(&m, &newer, 10).is_err());
        assert_eq!(load(&m).unwrap().as_deref(), Some(LONG));
    }

    #[test]
    fn a_failed_header_write_leaves_no_parts_behind() {
        let m = Memory {
            fail_set: Some("tokens"),
            ..Memory::default()
        };
        assert!(save(&m, LONG, 10).is_err());
        assert!(m.accounts().is_empty(), "{:?}", m.accounts());
    }

    /// Parts of generation `generation` as a save killed before writing
    /// its header leaves them: complete, but named by no header.
    fn orphan(m: &Memory, generation: u64, parts: usize) {
        for n in 0..parts {
            m.set(&part_account(generation, n), "secret").unwrap();
        }
    }

    #[test]
    fn logout_removes_parts_left_by_an_interrupted_save() {
        let m = Memory::default();
        orphan(&m, 1, 3);
        assert!(!delete(&m).unwrap());
        assert!(m.accounts().is_empty(), "{:?}", m.accounts());

        save(&m, LONG, 10).unwrap();
        orphan(&m, 2, 3);
        assert!(delete(&m).unwrap());
        assert!(m.accounts().is_empty(), "{:?}", m.accounts());

        m.set("tokens", "{}").unwrap();
        orphan(&m, 1, 3);
        assert!(delete(&m).unwrap());
        assert!(m.accounts().is_empty(), "{:?}", m.accounts());
    }

    #[test]
    fn a_save_after_an_interrupted_one_removes_its_leftover_parts() {
        let m = Memory::default();
        save(&m, LONG, 10).unwrap();
        // The interrupted save wrote more parts than the next one needs.
        orphan(&m, 2, 20);
        save(&m, "0123456789abc", 10).unwrap();
        assert_eq!(m.accounts(), ["tokens", "tokens.2.0", "tokens.2.1"]);
        assert_eq!(load(&m).unwrap().as_deref(), Some("0123456789abc"));

        orphan(&m, 3, 20);
        save(&m, "{}", 10).unwrap();
        assert_eq!(m.accounts(), ["tokens"]);
    }

    /// A keychain where another process saves `newer` right after this one
    /// reads the header, before it reads the first part.
    struct Racing {
        inner: Memory,
        newer: RefCell<Option<String>>,
    }

    impl Backend for Racing {
        fn get(&self, account: &str) -> Result<Option<String>> {
            if account != ACCOUNT
                && let Some(newer) = self.newer.borrow_mut().take()
            {
                save(&self.inner, &newer, 10)?;
            }
            self.inner.get(account)
        }

        fn set(&self, account: &str, secret: &str) -> Result<()> {
            self.inner.set(account, secret)
        }

        fn delete(&self, account: &str) -> Result<bool> {
            self.inner.delete(account)
        }
    }

    #[test]
    fn a_read_during_a_concurrent_save_returns_the_newer_sign_in() {
        let inner = Memory::default();
        save(&inner, LONG, 10).unwrap();
        let newer = LONG.replace("r-123", "r-456");
        let racing = Racing {
            inner,
            newer: RefCell::new(Some(newer.clone())),
        };
        assert_eq!(load(&racing).unwrap().as_deref(), Some(newer.as_str()));
    }

    #[test]
    fn a_missing_part_under_an_unchanged_header_is_an_error() {
        let m = Memory::default();
        save(&m, LONG, 10).unwrap();
        m.delete(&part_account(1, 1)).unwrap();
        let e = load(&m).unwrap_err().to_string();
        assert!(e.contains("part 1 of"), "{e}");
    }

    #[test]
    fn a_sign_in_stored_as_one_entry_still_loads() {
        let m = Memory::default();
        m.set("tokens", LONG).unwrap();
        assert_eq!(load(&m).unwrap().as_deref(), Some(LONG));
    }

    #[test]
    fn delete_removes_every_part() {
        let m = Memory::default();
        assert!(!delete(&m).unwrap());
        save(&m, LONG, 10).unwrap();
        assert!(delete(&m).unwrap());
        assert!(m.accounts().is_empty(), "{:?}", m.accounts());
        assert_eq!(load(&m).unwrap(), None);
    }

    /// The real Credential Manager, under a service of its own so a
    /// developer's sign-in is never touched.
    #[cfg(windows)]
    #[test]
    fn credential_manager_holds_a_sign_in_longer_than_one_entry() {
        let k = Keyring::with_service(&format!("yoto-cli-test-{}", std::process::id()));
        let text = format!(r#"{{"access_token":"{}"}}"#, "x".repeat(4000));
        save(&k, &text, ENTRY_LIMIT).unwrap();
        let loaded = load(&k);
        delete(&k).unwrap();
        assert_eq!(loaded.unwrap().as_deref(), Some(text.as_str()));
    }
}
