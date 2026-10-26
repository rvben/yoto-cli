//! The clispec v0.3 contract emitted by `yoto-cli schema`.
//!
//! Conforms to <https://clispec.dev/schema/v0.3.json> (validated by a test
//! against the vendored copy in `schemas/clispec-v0.3.json`). Keep this in sync
//! as commands, arguments and error kinds change.

use serde_json::{Value, json};

use crate::auth::DEFAULT_SCOPE;

/// The version of The CLI Spec this document conforms to.
pub const CLISPEC_VERSION: &str = "0.3";

/// The playlist-detail flags shared by `upload` and `content edit`.
fn metadata_args() -> Vec<Value> {
    vec![
        json!({"name": "--author", "type": "string[]", "description": "Author shown in the Yoto app; repeat the flag for several, in order, the first being the one Yoto shows; a name given twice is kept once. A single \"\" removes every author; \"\" beside a name is invalid_input."}),
        json!({"name": "--read-by", "type": "string[]", "description": "Narrator; repeat the flag for several, in order; a name given twice is kept once. A single \"\" removes every narrator; \"\" beside a name is invalid_input."}),
        json!({"name": "--description", "type": "string", "description": "Description; \"\" removes it."}),
        json!({"name": "--category", "type": "string", "enum": ["none", "stories", "music", "radio", "podcast", "sfx", "activities", "alarms"], "description": "Yoto category."}),
        json!({"name": "--min-age", "type": "integer", "minimum": 0, "maximum": 99, "description": "Youngest age in years, 0 to 99; may not exceed the maximum age."}),
        json!({"name": "--max-age", "type": "integer", "minimum": 0, "maximum": 99, "description": "Oldest age in years, 0 to 99."}),
        json!({"name": "--language", "type": "string", "description": "BCP 47 language tag such as nl or en-GB; \"\" removes it."}),
    ]
}

/// `args` followed by the metadata flags.
/// What every playlist change reports.
fn edit_output_fields() -> Value {
    json!([
        {"name": "card_id", "type": "string"},
        {"name": "title", "type": "string"},
        {"name": "change", "type": "string", "description": "What the command changes, in words."},
        {"name": "dry_run", "type": "boolean"},
        {"name": "changed", "type": "boolean", "description": "Whether the playlist was saved; false with --dry-run or when nothing differed."},
        {"name": "chapter_count", "type": "integer"},
        {"name": "track_count", "type": "integer"},
        {"name": "duration_seconds", "type": "number", "nullable": true},
        {"name": "metadata", "type": "object", "description": "The playlist details after the change (with --dry-run: as they would be), under their Yoto names (author, authors, narrators, description, category, minAge, maxAge, languages; --author sets authors and author, the first of them; --read-by sets narrators); a detail that is not set is absent."},
        {"name": "chapters", "type": "array", "description": "The chapters after the change (with --dry-run: as they would be).", "items": {"type": "object", "fields": [{"name": "number", "type": "integer"}, {"name": "key", "type": "string", "nullable": true}, {"name": "title", "type": "string", "nullable": true}, {"name": "track_count", "type": "integer"}, {"name": "icon", "type": "string", "nullable": true}]}}
    ])
}

/// The card ID, then `args`, then the confirmation flags.
fn chapter_args(args: Value) -> Value {
    let mut all = vec![
        json!({"name": "card_id", "type": "string", "required": true, "description": "Card ID from `content list`."}),
    ];
    all.extend(args.as_array().cloned().unwrap_or_default());
    all.push(json!({"name": "--dry-run", "type": "boolean", "default": false, "description": "Show the result without saving."}));
    all.push(json!({"name": "--yes", "type": "boolean", "default": false, "description": "Confirm the change."}));
    Value::Array(all)
}

fn with_fields(fields: Value, more: Value) -> Value {
    let mut all = fields.as_array().cloned().unwrap_or_default();
    all.extend(more.as_array().cloned().unwrap_or_default());
    Value::Array(all)
}

fn with_metadata_args(args: Value) -> Value {
    let mut all = args.as_array().cloned().unwrap_or_default();
    all.extend(metadata_args());
    Value::Array(all)
}

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
                    {"name": "chapters", "type": "array", "items": {"type": "object", "fields": [{"name": "number", "type": "integer", "description": "1-based position; the number the chapter commands take."}, {"name": "key", "type": "string"}, {"name": "title", "type": "string"}, {"name": "icon", "type": "string", "nullable": true, "description": "The chapter's 16x16 icon as a yoto:#<mediaId> reference; null when the playlist sets none."}, {"name": "tracks", "type": "array", "items": {"type": "object", "fields": [{"name": "key", "type": "string"}, {"name": "title", "type": "string"}, {"name": "duration_seconds", "type": "number", "nullable": true}, {"name": "format", "type": "string", "nullable": true}, {"name": "type", "type": "string", "nullable": true}]}}]}}
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
                "name": "content edit",
                "description": "Change a playlist's title (--title) or its details (--author, --read-by, --description, --category, --min-age, --max-age, --language; \"\" removes a text detail). The playlist is fetched, changed and saved under the same card ID, so cards linked to it keep working and its chapters are untouched. Needs at least one change; a change that leaves the playlist as it was saves nothing (changed false). Needs --yes unless --dry-run. Fails with not_found unless the playlist is one of yours.",
                "effects": "idempotent",
                "mutating": true,
                "cardinality": "single",
                "stability": "beta",
                "confirmation_bypass_arg": "--yes",
                "args": with_metadata_args(json!([
                    {"name": "card_id", "type": "string", "required": true, "description": "Card ID from `content list`."},
                    {"name": "--title", "type": "string", "description": "New playlist title, 1 to 140 characters."},
                    {"name": "--dry-run", "type": "boolean", "default": false, "description": "Show the result without saving."},
                    {"name": "--yes", "type": "boolean", "default": false, "description": "Confirm the change."}
                ])),
                "output_fields": edit_output_fields()
            },
            {
                "name": "content append",
                "description": "Add the audio of a folder, read as `upload` reads it (each file a chapter, each subfolder one chapter), as new chapters after the existing ones. Existing chapters keep their keys, so a player's place in them is kept, and the card ID stays the same. The playlist's total may not pass 100 tracks. An icons.json in the folder itself (not in its parent) numbers the new chapters from 1. Running it twice appends the chapters twice. Needs --yes unless --dry-run; progress goes to stderr.",
                "effects": "non_idempotent",
                "mutating": true,
                "cardinality": "single",
                "stability": "beta",
                "confirmation_bypass_arg": "--yes",
                "args": [
                    {"name": "card_id", "type": "string", "required": true, "description": "Card ID from `content list`."},
                    {"name": "dir", "type": "path", "required": true, "description": "Folder of audio files."},
                    {"name": "--icons", "type": "path", "description": "icons.json to use instead of the detected one; numbers count the new chapters from 1."},
                    {"name": "--icon", "type": "string", "description": "Icon for every new chapter the icons.json does not name: an image path or yoto:#<mediaId>."},
                    {"name": "--no-icons", "type": "boolean", "default": false, "description": "Ignore any icons.json."},
                    {"name": "--dry-run", "type": "boolean", "default": false, "description": "Show the new chapters without uploading."},
                    {"name": "--yes", "type": "boolean", "default": false, "description": "Confirm the change."},
                    {"name": "--transcode-timeout", "type": "integer", "default": 600, "description": "Seconds to wait for Yoto to transcode each track."}
                ],
                "output_fields": with_fields(edit_output_fields(), json!([
                    {"name": "new_chapters", "type": "array", "description": "Only with --dry-run: the chapters that would be added, numbered as in the playlist.", "items": {"type": "object", "fields": [{"name": "number", "type": "integer"}, {"name": "title", "type": "string"}, {"name": "track_count", "type": "integer"}, {"name": "icon", "type": "string", "nullable": true}]}},
                    {"name": "new_tracks", "type": "array", "description": "Only with --dry-run: the tracks that would be added, with `chapter` numbered as in the playlist.", "items": {"type": "object", "fields": [{"name": "number", "type": "integer"}, {"name": "chapter", "type": "integer"}, {"name": "title", "type": "string"}, {"name": "file", "type": "string"}, {"name": "bytes", "type": "integer"}, {"name": "format", "type": "string", "enum": ["mp3", "m4a", "wav", "flac", "ogg"]}, {"name": "duration_seconds", "type": "number", "nullable": true}, {"name": "on_end", "type": "string", "nullable": true, "enum": ["stop", "repeat"]}, {"name": "icon", "type": "string", "nullable": true, "description": "The chapter's icon."}]}},
                    {"name": "ignored", "type": "array", "items": {"type": "string"}, "description": "Only with --dry-run: files that are not audio."},
                    {"name": "uploaded", "type": "integer", "description": "Not with --dry-run: tracks uploaded."},
                    {"name": "reused", "type": "integer", "description": "Not with --dry-run: tracks Yoto already had."}
                ]))
            },
            {
                "name": "content chapter rename",
                "description": "Rename a chapter, and its track when the chapter has exactly one. Same card ID; needs --yes unless --dry-run.",
                "effects": "idempotent",
                "mutating": true,
                "cardinality": "single",
                "stability": "beta",
                "confirmation_bypass_arg": "--yes",
                "args": chapter_args(json!([
                    {"name": "number", "type": "integer", "required": true, "description": "Chapter number from `content show` (1-based)."},
                    {"name": "title", "type": "string", "required": true, "description": "New title, 1 to 140 characters."}
                ])),
                "output_fields": edit_output_fields()
            },
            {
                "name": "content chapter delete",
                "description": "Delete one or more chapters with their tracks. The remaining chapters keep their keys; number labels shown on the player follow the new positions. At least one chapter must remain (delete the playlist instead). Not idempotent: numbers refer to the current order. Needs --yes unless --dry-run.",
                "effects": "non_idempotent",
                "mutating": true,
                "cardinality": "single",
                "stability": "beta",
                "confirmation_bypass_arg": "--yes",
                "args": chapter_args(json!([
                    {"name": "numbers", "type": "integer[]", "required": true, "description": "Chapter numbers from `content show` (1-based)."}
                ])),
                "output_fields": edit_output_fields()
            },
            {
                "name": "content chapter move",
                "description": "Move a chapter so it becomes chapter TO; the others shift. Keys stay; number labels follow the new positions, other labels stay. Not idempotent. Needs --yes unless --dry-run.",
                "effects": "non_idempotent",
                "mutating": true,
                "cardinality": "single",
                "stability": "beta",
                "confirmation_bypass_arg": "--yes",
                "args": chapter_args(json!([
                    {"name": "from", "type": "integer", "required": true, "description": "Chapter number to move."},
                    {"name": "to", "type": "integer", "required": true, "description": "Position it moves to."}
                ])),
                "output_fields": edit_output_fields()
            },
            {
                "name": "content chapter icon",
                "description": "Set the 16x16 icon shown while a chapter plays, on the chapter and each of its tracks: a yoto:#<mediaId> from `icons list`, or a PNG, GIF or JPEG file that is uploaded as one of your icons (only after confirmation). Needs --yes unless --dry-run.",
                "effects": "idempotent",
                "mutating": true,
                "cardinality": "single",
                "stability": "beta",
                "confirmation_bypass_arg": "--yes",
                "args": chapter_args(json!([
                    {"name": "number", "type": "integer", "required": true, "description": "Chapter number from `content show` (1-based)."},
                    {"name": "icon", "type": "string", "required": true, "description": "yoto:#<mediaId> or an image path."}
                ])),
                "output_fields": edit_output_fields()
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
                "description": "Create a new playlist from a folder of audio files (MP3, M4A/AAC, WAV, FLAC, Ogg/Opus/Vorbis; detected from the bytes, not the extension; AIFF is refused because Yoto cannot transcode it) in name order, or from one playlist of a manifest.json (titles, order, cover and SHA-256 checks from the manifest). In a folder, each audio file is one chapter and each subfolder is one chapter of the audio files in it; in a manifest, consecutive entries with the same `chapter` form one chapter, and an entry's `on_end` (stop or repeat) sets what the player does when that track ends. Checks the Yoto limits (100 tracks, 100 MB and 60 min per track, 500 MB per playlist) before sending anything. Files Yoto already has (same SHA-256) are not uploaded again, so a failed run can simply be repeated. Chapter icons come from an icons.json beside the audio or in the book folder: an object whose keys are chapter numbers (1-based) or \"default\" and whose values are an image path relative to the JSON (PNG, GIF or JPEG; a 16x16 RGBA PNG or GIF is kept pixel-exact, anything else is converted by Yoto) or a yoto:#<mediaId> from `icons list`; chapters without one get Yoto's default icon. Every run without --replace creates another playlist; --replace updates an existing one in place, so cards linked to it keep working, and is safe to repeat. Linking the playlist to a physical card is done in the Yoto app. Needs --yes unless --dry-run; progress goes to stderr.",
                "effects": "non_idempotent",
                "mutating": true,
                "cardinality": "single",
                "stability": "beta",
                "confirmation_bypass_arg": "--yes",
                "args": with_metadata_args(json!([
                    {"name": "dir", "type": "path", "description": "Folder of audio files. A folder named `Kaart` or `Kaart N` takes its parent folder's name as title. Mutually exclusive with --manifest."},
                    {"name": "--manifest", "type": "path", "description": "manifest.json describing prepared books; requires --book."},
                    {"name": "--book", "type": "string", "description": "Book key in the manifest."},
                    {"name": "--playlist", "type": "string", "description": "Playlist name within the book; required when the book has several."},
                    {"name": "--title", "type": "string", "description": "Playlist title, 1 to 140 characters."},
                    {"name": "--cover", "type": "path", "description": "JPEG or PNG cover instead of the detected cover.jpg/cover.png."},
                    {"name": "--no-cover", "type": "boolean", "default": false, "description": "Upload no cover."},
                    {"name": "--icons", "type": "path", "description": "icons.json to use instead of the detected one."},
                    {"name": "--icon", "type": "string", "description": "Icon for every chapter the icons.json does not name: an image path or yoto:#<mediaId>."},
                    {"name": "--no-icons", "type": "boolean", "default": false, "description": "Ignore any icons.json."},
                    {"name": "--replace", "type": "string", "description": "Card ID of your playlist to replace instead of creating a new one."},
                    {"name": "--dry-run", "type": "boolean", "default": false, "description": "Check files, hashes and limits and print the plan; no sign-in or network needed."},
                    {"name": "--yes", "type": "boolean", "default": false, "description": "Confirm creating the playlist."},
                    {"name": "--transcode-timeout", "type": "integer", "default": 600, "description": "Seconds to wait for each track's transcode."}
                ])),
                "output_fields": [
                    {"name": "card_id", "type": "string", "description": "Created playlist. Absent with --dry-run."},
                    {"name": "title", "type": "string"},
                    {"name": "track_count", "type": "integer"},
                    {"name": "chapter_count", "type": "integer"},
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
                    {"name": "chapters", "type": "array", "description": "Only with --dry-run.", "items": {"type": "object", "fields": [{"name": "number", "type": "integer"}, {"name": "title", "type": "string"}, {"name": "track_count", "type": "integer"}, {"name": "icon", "type": "string", "nullable": true, "description": "Image path or yoto:# reference; null for the default icon."}]}},
                    {"name": "metadata", "type": "object", "description": "Only with --dry-run: the playlist details that would be set, under their Yoto names (author, authors, narrators, description, category, minAge, maxAge, languages; --author sets authors and author, the first of them; --read-by sets narrators)."},
                    {"name": "tracks", "type": "array", "description": "Only with --dry-run.", "items": {"type": "object", "fields": [{"name": "number", "type": "integer"}, {"name": "chapter", "type": "integer", "description": "Number of the chapter holding the track."}, {"name": "title", "type": "string"}, {"name": "file", "type": "string"}, {"name": "bytes", "type": "integer"}, {"name": "format", "type": "string", "enum": ["mp3", "m4a", "wav", "flac", "ogg"]}, {"name": "duration_seconds", "type": "number", "nullable": true}, {"name": "on_end", "type": "string", "nullable": true, "enum": ["stop", "repeat"]}, {"name": "icon", "type": "string", "nullable": true, "description": "The chapter's icon."}]}},
                    {"name": "ignored", "type": "array", "items": {"type": "string"}, "description": "Only with --dry-run: files in the folder (or a subfolder, as `sub/name`) that are not uploaded, and folders nested deeper than one level (as `sub/deeper/`)."}
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
            {"kind": "invalid_input", "exit_code": 1, "retryable": false, "description": "A local input is missing or unusable: folder, manifest, audio in no supported format, a minimum age above the maximum age, a language that is not a BCP 47 tag, \"\" beside a name in --author or --read-by, a size or SHA-256 that does not match the manifest, or a bad cover, icon or icons.json."},
            {"kind": "usage", "exit_code": 3, "retryable": false, "description": "Invalid command-line arguments (including an age outside 0 to 99), a missing client ID, or a manifest book needing --playlist."},
            {"kind": "not_authenticated", "exit_code": 4, "retryable": false, "description": "Not signed in, or the sign-in was rejected or can no longer be refreshed."},
            {"kind": "confirmation_required", "exit_code": 5, "retryable": false, "description": "A mutating command was run without --yes. Nothing was changed. A playlist change that would leave the playlist as it was needs no --yes: it succeeds with changed false."},
            {"kind": "limit_exceeded", "exit_code": 6, "retryable": false, "description": "The playlist breaks a Yoto limit."},
            {"kind": "not_found", "exit_code": 7, "retryable": false, "description": "The playlist (from content show/delete/edit/append, content chapter or upload --replace) does not exist or is not yours."},
            {"kind": "api_error", "exit_code": 8, "retryable": false, "description": "The Yoto API returned an error or an unexpected response."},
            {"kind": "network", "exit_code": 9, "retryable": true, "description": "No HTTP answer (DNS, TLS, connection, timeout)."},
            {"kind": "transcode_timeout", "exit_code": 10, "retryable": true, "description": "Yoto did not finish transcoding a track in time. Re-running reuses what was already uploaded."},
            {"kind": "auth_failed", "exit_code": 11, "retryable": false, "description": "The browser sign-in did not complete: refused, wrong state, timed out, port busy, or the code exchange was rejected."},
            {"kind": "token_store", "exit_code": 12, "retryable": false, "description": "Reading or writing the keychain, token file or config failed. On Linux the keychain is the Secret Service; without one, set YOTO_CLI_TOKEN_FILE."},
            {"kind": "conflict", "exit_code": 13, "retryable": true, "description": "The playlist changed (in the Yoto app or another command) while an edit was being prepared. Nothing was saved; re-running applies the edit to the current playlist. The playlist is read again just before saving; Yoto has no conditional save, so a change landing between that read and the save cannot be detected and is overwritten."},
            {"kind": "transcode_failed", "exit_code": 14, "retryable": false, "description": "Yoto reported that it could not transcode a track, usually because it cannot read the file. The message names the file and Yoto's reason. Nothing was saved."}
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
            Error::Conflict { message: m() },
            Error::TranscodeFailed { message: m() },
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
}
