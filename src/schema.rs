//! The clispec v0.3 contract emitted by `yoto-cli schema`.
//!
//! Conforms to <https://clispec.dev/schema/v0.3.json> (validated by a test
//! against the vendored copy in `schemas/clispec-v0.3.json`). Keep this in sync
//! as commands, arguments and error kinds change.

use serde_json::{Value, json};

use crate::auth::DEFAULT_SCOPE;

/// The version of The CLI Spec this document conforms to.
pub const CLISPEC_VERSION: &str = "0.3";

/// Build the clispec contract as a JSON value.
pub fn contract() -> Value {
    json!({
        "clispec": CLISPEC_VERSION,
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "description": concat!(
            env!("CARGO_PKG_DESCRIPTION"),
            " Independent tool using the Yoto Developer API. Environment: YOTO_CLIENT_ID (client ID for auth login), ",
            "YOTO_CLI_TOKEN_FILE (store tokens in this 0600 file instead of the OS keychain), YOTO_CLI_CONFIG_DIR (config directory), ",
            "YOTO_CLI_API_URL / YOTO_CLI_AUTH_URL (override the API and login base URLs)."
        ),
        "output": {"tty": "text", "piped": "json"},
        "global_args": [
            {
                "name": "--output",
                "short": "-o",
                "type": "string",
                "enum": ["auto", "json", "text"],
                "default": "auto",
                "description": "Output format. auto = text on a TTY, JSON when piped."
            }
        ],
        "commands": [
            {
                "name": "auth login",
                "description": "Sign in through the browser with OAuth Authorization Code + PKCE and a loopback redirect to http://127.0.0.1:<port>/callback, which must be registered for the client. Waits for the browser, not for the terminal. Stores tokens in the OS keychain and the client ID in the config file; signing in again replaces the stored tokens.",
                "effects": "idempotent",
                "mutating": true,
                "cardinality": "single",
                "stability": "beta",
                "args": [
                    {"name": "--client-id", "type": "string", "description": "Public client ID from https://dashboard.yoto.dev/. Falls back to YOTO_CLIENT_ID, then the remembered one."},
                    {"name": "--port", "type": "integer", "default": 8787, "description": "Loopback port for the redirect."},
                    {"name": "--scope", "type": "string", "default": DEFAULT_SCOPE, "description": "Space-separated OAuth scopes."},
                    {"name": "--no-browser", "type": "boolean", "default": false, "description": "Only print the sign-in link."},
                    {"name": "--timeout", "type": "integer", "default": 300, "description": "Seconds to wait for the redirect."}
                ],
                "output_fields": [
                    {"name": "signed_in", "type": "boolean"},
                    {"name": "client_id", "type": "string"},
                    {"name": "scope", "type": "string", "nullable": true},
                    {"name": "expires_at", "type": "integer", "nullable": true, "description": "Access token expiry, Unix seconds."},
                    {"name": "refreshable", "type": "boolean"},
                    {"name": "store", "type": "string"}
                ]
            },
            {
                "name": "auth status",
                "description": "Show the stored sign-in. Makes no network request; exits 0 when signed out (signed_in=false).",
                "effects": "read_only",
                "mutating": false,
                "cardinality": "single",
                "stability": "beta",
                "example": {"args": []},
                "output_fields": [
                    {"name": "signed_in", "type": "boolean"},
                    {"name": "client_id", "type": "string", "nullable": true},
                    {"name": "scope", "type": "string", "nullable": true, "description": "Only when signed in."},
                    {"name": "expires_at", "type": "integer", "nullable": true, "description": "Only when signed in."},
                    {"name": "access_token_valid", "type": "boolean", "description": "Only when signed in. false means the next command refreshes it."},
                    {"name": "refreshable", "type": "boolean", "description": "Only when signed in."},
                    {"name": "store", "type": "string"}
                ]
            },
            {
                "name": "auth logout",
                "description": "Delete the stored tokens. The client ID stays configured.",
                "effects": "idempotent",
                "mutating": true,
                "cardinality": "single",
                "stability": "beta",
                "output_fields": [
                    {"name": "signed_out", "type": "boolean"},
                    {"name": "removed", "type": "boolean", "description": "false when there was nothing stored."}
                ]
            },
            {
                "name": "content list",
                "description": "List your Make Your Own playlists. Output is {\"items\": [...]}.",
                "effects": "read_only",
                "mutating": false,
                "cardinality": "bounded",
                "stability": "beta",
                "output_fields": [
                    {"name": "card_id", "type": "string"},
                    {"name": "title", "type": "string"},
                    {"name": "created_at", "type": "string", "nullable": true},
                    {"name": "updated_at", "type": "string", "nullable": true},
                    {"name": "duration_seconds", "type": "number", "nullable": true}
                ]
            },
            {
                "name": "content show",
                "description": "Show one playlist with its chapters and tracks.",
                "effects": "read_only",
                "mutating": false,
                "cardinality": "single",
                "stability": "beta",
                "args": [
                    {"name": "card_id", "type": "string", "required": true, "description": "Card ID from `content list`."}
                ],
                "output_fields": [
                    {"name": "card_id", "type": "string"},
                    {"name": "title", "type": "string"},
                    {"name": "created_at", "type": "string", "nullable": true},
                    {"name": "updated_at", "type": "string", "nullable": true},
                    {"name": "duration_seconds", "type": "number", "nullable": true},
                    {"name": "cover_url", "type": "string", "nullable": true},
                    {"name": "chapters", "type": "array", "items": {"type": "object", "fields": [{"name": "key", "type": "string"}, {"name": "title", "type": "string"}, {"name": "icon", "type": "string", "nullable": true, "description": "The chapter's 16x16 icon as a yoto:#<mediaId> reference; null when the playlist sets none."}, {"name": "tracks", "type": "array", "items": {"type": "object", "fields": [{"name": "key", "type": "string"}, {"name": "title", "type": "string"}, {"name": "duration_seconds", "type": "number", "nullable": true}, {"name": "format", "type": "string", "nullable": true}, {"name": "type", "type": "string", "nullable": true}]}}]}}
                ]
            },
            {
                "name": "content delete",
                "description": "Permanently delete one of your playlists. Without --yes it only looks the playlist up and fails with confirmation_required. Deleting it again fails with not_found.",
                "effects": "idempotent",
                "mutating": true,
                "cardinality": "single",
                "stability": "beta",
                "confirmation_bypass_arg": "--yes",
                "args": [
                    {"name": "card_id", "type": "string", "required": true, "description": "Card ID from `content list`."},
                    {"name": "--yes", "type": "boolean", "default": false, "description": "Confirm the deletion."}
                ],
                "output_fields": [
                    {"name": "card_id", "type": "string"},
                    {"name": "title", "type": "string"},
                    {"name": "deleted", "type": "boolean"}
                ]
            },
            {
                "name": "icons list",
                "description": "List Yoto's public 16x16 icons (or, with --mine, your uploads), keeping those whose title or tags contain every --query word. Use the `icon` value in an icons.json or with upload --icon. Output is {\"items\": [...], \"total\": <unfiltered count>}.",
                "effects": "read_only",
                "mutating": false,
                "cardinality": "bounded",
                "stability": "beta",
                "args": [
                    {"name": "--query", "type": "string[]", "description": "Words that must all occur in the title or tags (case-insensitive)."},
                    {"name": "--mine", "type": "boolean", "default": false, "description": "List your own uploaded icons."}
                ],
                "output_fields": [
                    {"name": "icon", "type": "string", "description": "yoto:#<mediaId>"},
                    {"name": "title", "type": "string", "nullable": true},
                    {"name": "tags", "type": "array", "items": {"type": "string"}},
                    {"name": "url", "type": "string", "nullable": true, "description": "PNG preview."}
                ]
            },
            {
                "name": "upload",
                "description": "Create a new playlist (one chapter per track) from a folder of MP3/M4A files in file-name order, or from one playlist of a manifest.json (titles, order, cover and SHA-256 checks from the manifest). Checks the Yoto limits (100 tracks, 100 MB and 60 min per track, 500 MB per playlist) before sending anything. Files Yoto already has (same SHA-256) are not uploaded again, so a failed run can simply be repeated. Chapter icons come from an icons.json beside the audio or in the book folder: an object whose keys are track numbers (1-based) or \"default\" and whose values are an image path relative to the JSON (PNG, GIF or JPEG; a 16x16 RGBA PNG or GIF is kept pixel-exact, anything else is converted by Yoto) or a yoto:#<mediaId> from `icons list`; tracks without one get Yoto's default icon. Every run without --replace creates another playlist; --replace updates an existing one in place, so cards linked to it keep working, and is safe to repeat. Linking the playlist to a physical card is done in the Yoto app. Needs --yes unless --dry-run; progress goes to stderr.",
                "effects": "non_idempotent",
                "mutating": true,
                "cardinality": "single",
                "stability": "beta",
                "confirmation_bypass_arg": "--yes",
                "args": [
                    {"name": "dir", "type": "path", "description": "Folder of audio files. A folder named `Kaart` or `Kaart N` takes its parent folder's name as title. Mutually exclusive with --manifest."},
                    {"name": "--manifest", "type": "path", "description": "manifest.json describing prepared books; requires --book."},
                    {"name": "--book", "type": "string", "description": "Book key in the manifest."},
                    {"name": "--playlist", "type": "string", "description": "Playlist name within the book; required when the book has several."},
                    {"name": "--title", "type": "string", "description": "Playlist title, 1 to 140 characters."},
                    {"name": "--cover", "type": "path", "description": "JPEG or PNG cover instead of the detected cover.jpg/cover.png."},
                    {"name": "--no-cover", "type": "boolean", "default": false, "description": "Upload no cover."},
                    {"name": "--icons", "type": "path", "description": "icons.json to use instead of the detected one."},
                    {"name": "--icon", "type": "string", "description": "Icon for every track the icons.json does not name: an image path or yoto:#<mediaId>."},
                    {"name": "--no-icons", "type": "boolean", "default": false, "description": "Ignore any icons.json."},
                    {"name": "--replace", "type": "string", "description": "Card ID of your playlist to replace instead of creating a new one."},
                    {"name": "--dry-run", "type": "boolean", "default": false, "description": "Check files, hashes and limits and print the plan; no sign-in or network needed."},
                    {"name": "--yes", "type": "boolean", "default": false, "description": "Confirm creating the playlist."},
                    {"name": "--transcode-timeout", "type": "integer", "default": 600, "description": "Seconds to wait for each track's transcode."}
                ],
                "output_fields": [
                    {"name": "card_id", "type": "string", "description": "Created playlist. Absent with --dry-run."},
                    {"name": "title", "type": "string"},
                    {"name": "track_count", "type": "integer"},
                    {"name": "uploaded", "type": "integer", "description": "Tracks sent. Absent with --dry-run."},
                    {"name": "reused", "type": "integer", "description": "Tracks Yoto already had. Absent with --dry-run."},
                    {"name": "duration_seconds", "type": "number", "nullable": true},
                    {"name": "bytes", "type": "integer"},
                    {"name": "cover", "type": "boolean", "description": "Whether a cover was set. Absent with --dry-run."},
                    {"name": "cover_file", "type": "string", "nullable": true, "description": "Only with --dry-run: the cover that would be uploaded."},
                    {"name": "custom_icons", "type": "integer", "description": "Distinct non-default icons used. Absent with --dry-run."},
                    {"name": "replaced", "type": "boolean", "description": "Whether an existing playlist was replaced. Absent with --dry-run."},
                    {"name": "replace", "type": "string", "nullable": true, "description": "Only with --dry-run: the playlist that would be replaced."},
                    {"name": "icons_file", "type": "string", "nullable": true, "description": "Only with --dry-run: the icons.json used."},
                    {"name": "dry_run", "type": "boolean", "description": "Only with --dry-run."},
                    {"name": "tracks", "type": "array", "description": "Only with --dry-run.", "items": {"type": "object", "fields": [{"name": "number", "type": "integer"}, {"name": "title", "type": "string"}, {"name": "file", "type": "string"}, {"name": "bytes", "type": "integer"}, {"name": "format", "type": "string", "enum": ["mp3", "m4a"]}, {"name": "duration_seconds", "type": "number", "nullable": true}, {"name": "icon", "type": "string", "nullable": true, "description": "Image path or yoto:# reference; null for the default icon."}]}},
                    {"name": "ignored", "type": "array", "items": {"type": "string"}, "description": "Only with --dry-run: files in the folder that are not uploaded."}
                ]
            },
            {
                "name": "schema",
                "description": "Print this clispec contract as JSON.",
                "effects": "read_only",
                "mutating": false,
                "cardinality": "single",
                "stdout_schema": {"$ref": "https://clispec.dev/schema/v0.3.json"},
                "stability": "stable"
            },
            {
                "name": "completions",
                "description": "Generate a shell completion script.",
                "effects": "read_only",
                "mutating": false,
                "output_kind": "opaque",
                "media_type": "text/plain",
                "stability": "stable",
                "args": [
                    {"name": "shell", "type": "string", "required": true, "enum": ["bash", "zsh", "fish", "powershell", "elvish"], "description": "Target shell."}
                ]
            }
        ],
        "errors": [
            {"kind": "invalid_input", "exit_code": 1, "retryable": false, "description": "A local input is missing or unusable: folder, manifest, audio that is not MP3/MP4, a size or SHA-256 that does not match the manifest, or a bad cover, icon or icons.json."},
            {"kind": "usage", "exit_code": 3, "retryable": false, "description": "Invalid command-line arguments, a missing client ID, or a manifest book needing --playlist."},
            {"kind": "not_authenticated", "exit_code": 4, "retryable": false, "description": "Not signed in, or the sign-in was rejected or can no longer be refreshed."},
            {"kind": "confirmation_required", "exit_code": 5, "retryable": false, "description": "A mutating command was run without --yes. Nothing was changed."},
            {"kind": "limit_exceeded", "exit_code": 6, "retryable": false, "description": "The playlist breaks a Yoto limit."},
            {"kind": "not_found", "exit_code": 7, "retryable": false, "description": "The playlist (from content show/delete or upload --replace) does not exist or is not yours."},
            {"kind": "api_error", "exit_code": 8, "retryable": false, "description": "The Yoto API returned an error or an unexpected response."},
            {"kind": "network", "exit_code": 9, "retryable": true, "description": "No HTTP answer (DNS, TLS, connection, timeout)."},
            {"kind": "transcode_timeout", "exit_code": 10, "retryable": true, "description": "Yoto did not finish transcoding a track in time. Re-running reuses what was already uploaded."},
            {"kind": "auth_failed", "exit_code": 11, "retryable": false, "description": "The browser sign-in did not complete: refused, wrong state, timed out, port busy, or the code exchange was rejected."},
            {"kind": "token_store", "exit_code": 12, "retryable": false, "description": "Reading or writing the keychain, token file or config failed. On Linux the keychain is the Secret Service; without one, set YOTO_CLI_TOKEN_FILE."}
        ]
    })
}

/// The contract as a pretty-printed JSON string.
pub fn contract_json() -> String {
    serde_json::to_string_pretty(&contract()).expect("contract serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    /// Every error the code can return, so the schema cannot drift from `Error::kind`.
    fn every_error() -> Vec<Error> {
        let m = || "m".to_string();
        vec![
            Error::Usage { message: m() },
            Error::InvalidInput { message: m() },
            Error::LimitExceeded { message: m() },
            Error::NotAuthenticated { message: m() },
            Error::AuthFailed { message: m() },
            Error::ConfirmationRequired { message: m() },
            Error::NotFound { message: m() },
            Error::Api {
                status: 500,
                message: m(),
            },
            Error::Network { message: m() },
            Error::TranscodeTimeout { message: m() },
            Error::TokenStore { message: m() },
        ]
    }

    #[test]
    fn every_error_kind_is_declared_with_its_exit_code() {
        let declared = contract()["errors"].as_array().unwrap().clone();
        let errors = every_error();
        assert_eq!(
            declared.len(),
            errors.len(),
            "schema declares a kind the code never returns, or misses one"
        );
        for err in errors {
            let entry = declared
                .iter()
                .find(|d| d["kind"] == err.kind())
                .unwrap_or_else(|| panic!("{} is not declared", err.kind()));
            assert_eq!(entry["exit_code"], err.exit_code(), "{}", err.kind());
        }
    }

    #[test]
    fn every_cli_command_is_declared() {
        let names: Vec<String> = contract()["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect();
        for expected in [
            "auth login",
            "auth status",
            "auth logout",
            "content list",
            "content show",
            "content delete",
            "icons list",
            "upload",
            "schema",
            "completions",
        ] {
            assert!(names.iter().any(|n| n == expected), "{expected} missing");
        }
    }
}
