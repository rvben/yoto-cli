//! yoto-cli library: sign-in, the Yoto API client, upload planning, and the
//! command implementations the binary dispatches to.
//!
//! Every command returns an [`Output`] carrying both the JSON document
//! (piped / `--output json`) and the human text (TTY / `--output text`).

// The schema's nested `json!` literal needs more than the default 128.
#![recursion_limit = "256"]

pub mod api;
pub mod auth;
pub mod config;
pub mod edit;
pub mod error;
pub mod icons;
pub mod keychain;
pub mod metadata;
pub mod plan;
pub mod schema;
pub mod upload;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

pub use error::{Error, Result};

use config::{Config, Endpoints, TokenStore};

/// The output format the binary resolved from `--output` and the TTY state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Text,
}

/// A command result in both renderings.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub json: Value,
    pub text: String,
}

impl Output {
    pub fn render(&self, format: OutputFormat) -> String {
        match format {
            OutputFormat::Json => self.json.to_string(),
            OutputFormat::Text => self.text.trim_end().to_string(),
        }
    }
}

/// Where to talk to and where state lives; built from the environment.
pub struct Context {
    pub endpoints: Endpoints,
    pub store: TokenStore,
    pub config_dir: PathBuf,
}

impl Context {
    pub fn from_env() -> Result<Self> {
        Ok(Context {
            endpoints: Endpoints::from_env(),
            store: TokenStore::from_env(),
            config_dir: config::config_dir()?,
        })
    }

    fn client(&self) -> Result<api::Client> {
        let http = api::http_client();
        let credentials = auth::StoredCredentials {
            store: self.store.clone(),
            http: http.clone(),
            endpoints: self.endpoints.clone(),
        };
        api::Client::signed_in(http, &self.endpoints.api, Box::new(credentials))
    }
}

/// `1:05:09` / `4:09` for a length in seconds.
pub fn format_duration(seconds: f64) -> String {
    let s = seconds.round() as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn format_mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

pub struct LoginOptions {
    pub client_id: Option<String>,
    pub port: u16,
    pub scope: String,
    pub open_browser: bool,
    pub timeout: Duration,
}

/// `auth login`: browser sign-in with PKCE, tokens into the store, client ID into the config.
pub fn auth_login(
    ctx: &Context,
    opts: &LoginOptions,
    mut log: impl FnMut(String),
) -> Result<Output> {
    let mut cfg = Config::load(&ctx.config_dir)?;
    let client_id = opts
        .client_id
        .clone()
        .or_else(|| std::env::var("YOTO_CLIENT_ID").ok().filter(|v| !v.is_empty()))
        .or_else(|| cfg.client_id.clone())
        .ok_or_else(|| {
            Error::usage(
                "no client ID: create a public client at https://dashboard.yoto.dev/ and pass --client-id (it is remembered)",
            )
        })?;
    let listener = auth::bind_callback(opts.port)?;
    let redirect = auth::redirect_uri(listener.local_addr().map(|a| a.port()).unwrap_or(opts.port));
    let pkce = auth::Pkce::generate();
    let state = auth::new_state();
    let url = auth::authorize_url(
        &ctx.endpoints,
        &client_id,
        &redirect,
        &opts.scope,
        &pkce,
        &state,
    );
    log(format!("Open this page to sign in to Yoto:\n\n  {url}\n"));
    if opts.open_browser && !auth::open_browser(&url) {
        log("(could not open a browser; open the link above yourself)".into());
    }
    log(format!("Waiting for the redirect to {redirect} ..."));
    let code = auth::wait_for_code(&listener, &state, opts.timeout)?;
    let http = api::http_client();
    let tokens = auth::exchange_code(
        &http,
        &ctx.endpoints,
        &client_id,
        &code,
        &pkce.verifier,
        &redirect,
    )?;
    {
        // Under the lock, so a refresh in flight cannot overwrite the new sign-in.
        let _lock = ctx.store.lock()?;
        ctx.store.save(&tokens)?;
    }
    if cfg.client_id.as_deref() != Some(client_id.as_str()) {
        cfg.client_id = Some(client_id.clone());
        cfg.save(&ctx.config_dir)?;
    }
    Ok(Output {
        text: format!(
            "Signed in. Tokens are stored in the {}.{}",
            ctx.store.describe(),
            if tokens.refresh_token.is_none() {
                " No refresh token was issued, so you will need to sign in again when it expires."
            } else {
                ""
            }
        ),
        json: json!({
            "signed_in": true,
            "client_id": client_id,
            "scope": tokens.scope,
            "expires_at": tokens.expires_at,
            "refreshable": tokens.refresh_token.is_some(),
            "store": ctx.store.describe(),
        }),
    })
}

/// `auth status`: what is stored locally. Makes no network request.
pub fn auth_status(ctx: &Context) -> Result<Output> {
    let cfg = Config::load(&ctx.config_dir)?;
    let tokens = ctx.store.load()?;
    let now = auth::now_unix();
    let json = match &tokens {
        None => json!({
            "signed_in": false,
            "client_id": cfg.client_id,
            "store": ctx.store.describe(),
        }),
        Some(t) => json!({
            "signed_in": true,
            "client_id": t.client_id,
            "scope": t.scope,
            "expires_at": t.expires_at,
            "access_token_valid": auth::is_fresh(t, now),
            "refreshable": t.refresh_token.is_some(),
            "store": ctx.store.describe(),
        }),
    };
    let text = match &tokens {
        None => "Not signed in. Run `yoto-cli auth login`.".to_string(),
        Some(t) => {
            let access = match t.expires_at {
                Some(exp) if exp > now => format!(
                    "access token valid for {} more",
                    format_duration((exp - now) as f64)
                ),
                Some(_) => "access token expired".into(),
                None => "access token expiry unknown".into(),
            };
            let refresh = if t.refresh_token.is_some() {
                "refreshes automatically"
            } else {
                "no refresh token"
            };
            format!(
                "Signed in (client {}); {access}, {refresh}.\nScope: {}\nStored in the {}.",
                t.client_id,
                t.scope.as_deref().unwrap_or("unknown"),
                ctx.store.describe()
            )
        }
    };
    Ok(Output { json, text })
}

/// `auth logout`: forget the stored tokens (the client ID stays configured).
pub fn auth_logout(ctx: &Context) -> Result<Output> {
    // Under the lock, so a refresh in flight cannot save its tokens back.
    let _lock = ctx.store.lock()?;
    let removed = ctx.store.delete()?;
    Ok(Output {
        json: json!({"signed_out": true, "removed": removed}),
        text: if removed {
            "Signed out.".into()
        } else {
            "Was not signed in.".into()
        },
    })
}

fn card_summary(card: &Value) -> Value {
    json!({
        "card_id": card.get("cardId"),
        "title": card.get("title"),
        "created_at": card.get("createdAt"),
        "updated_at": card.get("updatedAt"),
        "duration_seconds": card.pointer("/metadata/media/duration"),
    })
}

/// `content list`: the signed-in user's playlists.
pub fn content_list(ctx: &Context) -> Result<Output> {
    let cards = ctx.client()?.list_mine()?;
    let items: Vec<Value> = cards.iter().map(card_summary).collect();
    let mut text = String::new();
    if items.is_empty() {
        text.push_str("No Make Your Own playlists.");
    }
    for item in &items {
        let duration = item["duration_seconds"]
            .as_f64()
            .map(format_duration)
            .unwrap_or_else(|| "n/a".into());
        text.push_str(&format!(
            "{:<6} {:>8}  {}\n",
            item["card_id"].as_str().unwrap_or("?"),
            duration,
            item["title"].as_str().unwrap_or("")
        ));
    }
    Ok(Output {
        json: json!({"items": items}),
        text,
    })
}

/// `content show`: one playlist with its chapters and tracks.
pub fn content_show(ctx: &Context, card_id: &str) -> Result<Output> {
    let card = ctx.client()?.get_content(card_id)?;
    let chapters: Vec<Value> = card
        .pointer("/content/chapters")
        .and_then(Value::as_array)
        .map(|chs| {
            chs.iter()
                .enumerate()
                .map(|(i, ch)| {
                    json!({
                        "number": i + 1,
                        "key": ch.get("key"),
                        "title": ch.get("title"),
                        "icon": ch.pointer("/display/icon16x16"),
                        "tracks": ch.get("tracks").and_then(Value::as_array).map(|ts| ts.iter().map(|t| json!({
                            "key": t.get("key"),
                            "title": t.get("title"),
                            "duration_seconds": t.get("duration"),
                            "format": t.get("format"),
                            "type": t.get("type"),
                        })).collect::<Vec<_>>()).unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut json = card_summary(&card);
    json["cover_url"] = card
        .pointer("/metadata/cover/imageL")
        .cloned()
        .unwrap_or(Value::Null);
    json["chapters"] = Value::Array(chapters);
    let mut text = format!(
        "{} ({})\n",
        json["title"].as_str().unwrap_or(""),
        json["card_id"].as_str().unwrap_or(card_id)
    );
    for (i, ch) in json["chapters"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let secs: Option<f64> = ch["tracks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| t["duration_seconds"].as_f64())
            .sum();
        text.push_str(&format!(
            "{:>3}. {:>8}  {}\n",
            i + 1,
            secs.map(format_duration).unwrap_or_else(|| "n/a".into()),
            ch["title"].as_str().unwrap_or("")
        ));
    }
    Ok(Output { json, text })
}

/// `content delete`: delete one of your playlists. Requires `yes`.
pub fn content_delete(ctx: &Context, card_id: &str, yes: bool) -> Result<Output> {
    let client = ctx.client()?;
    let card = client.get_content(card_id)?;
    let title = card
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if !yes {
        return Err(Error::ConfirmationRequired {
            message: format!(
                "deleting playlist {card_id} ({title:?}) cannot be undone; re-run with --yes"
            ),
        });
    }
    client.delete_content(card_id)?;
    Ok(Output {
        json: json!({"card_id": card_id, "title": title, "deleted": true}),
        text: format!("Deleted {title:?} ({card_id})."),
    })
}

/// Fail unless `card_id` is one of the signed-in user's own playlists. Yoto
/// also serves playlists the user may only read, so the check is membership
/// in the user's own list, not a readable GET.
fn ensure_mine(client: &api::Client, card_id: &str) -> Result<()> {
    if client
        .list_mine()?
        .iter()
        .any(|c| c["cardId"].as_str() == Some(card_id))
    {
        Ok(())
    } else {
        Err(Error::NotFound {
            message: format!("playlist {card_id} is not one of your playlists"),
        })
    }
}

/// A change to an existing playlist that needs no new audio.
pub enum Change<'a> {
    Details {
        title: Option<&'a str>,
        metadata: &'a metadata::Metadata,
    },
    RenameChapter {
        number: usize,
        title: &'a str,
    },
    DeleteChapters(&'a [usize]),
    MoveChapter {
        from: usize,
        to: usize,
    },
    /// A `yoto:#` reference or an image file.
    ChapterIcon {
        number: usize,
        icon: &'a str,
    },
}

/// Whether a playlist change is only shown, still needs confirming, or is made.
#[derive(Debug, Clone, Copy)]
pub struct Confirm {
    pub dry_run: bool,
    pub yes: bool,
}

/// `content edit` and `content chapter ...`: change a playlist in place.
/// The playlist keeps its card ID, so cards linked to it keep working.
pub fn content_edit(
    ctx: &Context,
    card_id: &str,
    change: &Change,
    confirm: Confirm,
    mut log: impl FnMut(String),
) -> Result<Output> {
    let icon = match change {
        Change::ChapterIcon { icon, .. } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            Some(icons::Icon::parse(icon, &cwd)?)
        }
        Change::Details { title, metadata } => {
            if title.is_none() && metadata.is_empty() {
                return Err(Error::usage(
                    "nothing to change; give --title or a detail such as --author",
                ));
            }
            metadata.validate()?;
            None
        }
        _ => None,
    };
    let client = ctx.client()?;
    ensure_mine(&client, card_id)?;
    let before = edit::editable(&client.get_content(card_id)?)?;
    let apply = |card: &mut Value, icon_ref: Option<&str>| -> Result<String> {
        Ok(match change {
            Change::Details { title, metadata } => {
                let mut set = Vec::new();
                if let Some(t) = title {
                    edit::set_title(card, t)?;
                    set.push(format!("title to {:?}", t.trim()));
                }
                edit::apply_metadata(card, metadata)?;
                set.extend(
                    metadata
                        .to_json()
                        .as_object()
                        .into_iter()
                        .flatten()
                        // `authors` mirrors `author`, and an emptied list is
                        // reported as a removal below.
                        .filter(|(field, value)| {
                            *field != "authors" && value.as_array().is_none_or(|a| !a.is_empty())
                        })
                        .map(|(field, value)| format!("{field} to {value}")),
                );
                let removed = metadata_removals(metadata);
                let mut parts = Vec::new();
                if !set.is_empty() {
                    parts.push(format!("set {}", set.join(", ")));
                }
                if !removed.is_empty() {
                    parts.push(format!("remove {}", removed.join(", ")));
                }
                parts.join("; ")
            }
            Change::RenameChapter { number, title } => {
                edit::rename_chapter(card, *number, title)?;
                format!("rename chapter {number} to {:?}", title.trim())
            }
            Change::DeleteChapters(numbers) => {
                edit::delete_chapters(card, numbers)?;
                let list: Vec<String> = numbers.iter().map(usize::to_string).collect();
                format!(
                    "delete chapter{} {}",
                    if numbers.len() == 1 { "" } else { "s" },
                    list.join(", ")
                )
            }
            Change::MoveChapter { from, to } => {
                edit::move_chapter(card, *from, *to)?;
                format!("move chapter {from} to position {to}")
            }
            Change::ChapterIcon { number, .. } => {
                let icon = icon.as_ref().expect("parsed above");
                edit::set_chapter_icon(card, *number, icon_ref.unwrap_or(&icon.describe()))?;
                format!("set the icon of chapter {number} to {}", icon.describe())
            }
        })
    };
    // Only removing tracks changes the totals; any other edit keeps them as
    // Yoto stored them.
    let recompute = |card: &mut Value| {
        if matches!(change, Change::DeleteChapters(_)) {
            edit::recompute_media(card);
        }
    };
    let mut after = before.clone();
    let what = apply(&mut after, None)?;
    recompute(&mut after);
    if after == before {
        let mut out = edit_output(card_id, &after, &what, EditState::Unchanged);
        out.json["dry_run"] = json!(confirm.dry_run);
        return Ok(out);
    }
    if confirm.dry_run {
        return Ok(edit_output(card_id, &after, &what, EditState::DryRun));
    }
    if !confirm.yes {
        return Err(confirmation(card_id, &before, &what));
    }
    if let Some(icon @ icons::Icon::File(_)) = &icon {
        let reference = upload::resolve_icon(&client, icon, &mut Default::default(), &mut log)?;
        after = before.clone();
        apply(&mut after, Some(&reference))?;
        recompute(&mut after);
    }
    save(&client, card_id, &before, &after, &mut log)?;
    Ok(edit_output(card_id, &after, &what, EditState::Saved))
}

/// The text fields an edit removes by giving them as "".
fn metadata_removals(m: &metadata::Metadata) -> Vec<String> {
    [
        ("author", &m.author),
        ("narrators", &m.read_by),
        ("description", &m.description),
        ("languages", &m.language),
    ]
    .into_iter()
    .filter(|(_, v)| v.as_deref().is_some_and(|v| v.trim().is_empty()))
    .map(|(f, _)| f.to_string())
    .collect()
}

pub struct AppendOptions<'a> {
    pub dir: &'a Path,
    /// An icons.json to use instead of the detected one; its chapter numbers
    /// count the appended chapters from 1.
    pub icons: Option<PathBuf>,
    /// Icon for every appended chapter the icons.json does not name.
    pub icon: Option<String>,
    pub no_icons: bool,
    pub confirm: Confirm,
    pub transcode_timeout: Duration,
}

/// `content append`: add a folder's audio as new chapters at the end of a
/// playlist, keeping its card ID and its existing chapters.
pub fn content_append(
    ctx: &Context,
    card_id: &str,
    opts: &AppendOptions,
    mut log: impl FnMut(String),
) -> Result<Output> {
    // Only an icons.json in the appended folder itself numbers the appended
    // chapters; one in the book folder numbers the book's own chapters.
    let detected = icons::find(&[opts.dir]);
    let icons_file = match (&opts.icons, opts.no_icons, &detected) {
        (_, true, _) => plan::IconsFile::Ignore,
        (Some(path), false, _) => plan::IconsFile::Path(path),
        (None, false, Some(path)) => plan::IconsFile::Path(path),
        (None, false, None) => plan::IconsFile::Ignore,
    };
    let mut plan = plan::from_dir(opts.dir, icons_file)?;
    if let Some(spec) = &opts.icon {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        plan.icons.default = Some(icons::Icon::parse(spec, &cwd)?);
    }
    plan.check_audio_limits()?;
    let client = ctx.client()?;
    ensure_mine(&client, card_id)?;
    let before = edit::editable(&client.get_content(card_id)?)?;
    let total = edit::track_count(&before) + plan.tracks.len();
    if total > plan::MAX_TRACKS {
        return Err(Error::LimitExceeded {
            message: format!(
                "the playlist has {} tracks; adding {} makes {total}, above Yoto's limit of {}",
                edit::track_count(&before),
                plan.tracks.len(),
                plan::MAX_TRACKS
            ),
        });
    }
    let bytes = edit::known_bytes(&before) + plan.total_bytes();
    if bytes > plan::MAX_PLAYLIST_BYTES {
        return Err(Error::LimitExceeded {
            message: format!(
                "the playlist holds {} bytes; adding {} makes {bytes}, above Yoto's limit of {}",
                edit::known_bytes(&before),
                plan.total_bytes(),
                plan::MAX_PLAYLIST_BYTES
            ),
        });
    }
    let existing = edit::chapter_count(&before);
    let what = format!(
        "append {} chapter{} ({} track{}, {})",
        plan.chapters.len(),
        if plan.chapters.len() == 1 { "" } else { "s" },
        plan.tracks.len(),
        if plan.tracks.len() == 1 { "" } else { "s" },
        format_mb(plan.total_bytes())
    );
    if opts.confirm.dry_run {
        let mut out = edit_output(card_id, &before, &what, EditState::DryRun);
        let mut new = plan.to_json();
        // Number the planned chapters as they will appear in the playlist.
        for ch in new["chapters"].as_array_mut().into_iter().flatten() {
            let n = ch["number"].as_u64().unwrap_or(0) as usize;
            ch["number"] = json!(existing + n);
        }
        for t in new["tracks"].as_array_mut().into_iter().flatten() {
            let n = t["chapter"].as_u64().unwrap_or(0) as usize;
            t["chapter"] = json!(existing + n);
        }
        out.text.push_str("New chapters:\n");
        for ch in new["chapters"].as_array().into_iter().flatten() {
            out.text.push_str(&format!(
                "{:>3}. {}\n",
                ch["number"].as_u64().unwrap_or(0),
                ch["title"].as_str().unwrap_or("")
            ));
        }
        if !plan.ignored.is_empty() {
            out.text
                .push_str(&format!("Ignored: {}\n", plan.ignored.join(", ")));
        }
        // Describe the playlist as it would be, existing chapters first.
        let new_chapters = new["chapters"].as_array().cloned().unwrap_or_default();
        if let Some(chapters) = out.json["chapters"].as_array_mut() {
            chapters.extend(new_chapters.iter().map(|ch| {
                let mut ch = ch.clone();
                ch["key"] = Value::Null;
                ch
            }));
        }
        out.json["chapter_count"] = json!(existing + plan.chapters.len());
        out.json["track_count"] = json!(total);
        out.json["duration_seconds"] = match (
            before
                .pointer("/metadata/media/duration")
                .and_then(Value::as_f64),
            plan.total_seconds(),
        ) {
            (Some(a), Some(b)) => json!(a + b),
            _ => Value::Null,
        };
        out.json["new_chapters"] = new["chapters"].take();
        out.json["new_tracks"] = new["tracks"].take();
        out.json["ignored"] = json!(plan.ignored);
        return Ok(out);
    }
    if !opts.confirm.yes {
        return Err(confirmation(card_id, &before, &what));
    }
    let n = plan.tracks.len();
    let hashes = plan::hash_tracks(&plan, |i, t| {
        log(format!("[{}/{n}] checking {}", i + 1, t.title))
    })?;
    let icon_refs = upload::upload_icons(&client, &plan, &mut log)?;
    let done = upload::upload_tracks(&client, &plan, &hashes, opts.transcode_timeout, &mut log)?;
    let bodies: Vec<upload::ChapterBody> = plan
        .chapters
        .iter()
        .zip(&icon_refs)
        .map(|(c, icon)| upload::ChapterBody {
            title: &c.title,
            icon,
            tracks: &done[c.tracks.clone()],
        })
        .collect();
    let mut body =
        upload::content_body(&plan.title, &bodies, None, &metadata::Metadata::default())?;
    let new: Vec<Value> = match body["content"]["chapters"].take() {
        Value::Array(chs) => chs,
        _ => unreachable!("content_body builds a chapters array"),
    };
    let mut after = before.clone();
    edit::append_chapters(&mut after, new)?;
    plan::check_transcoded_bytes(edit::known_bytes(&after))?;
    edit::recompute_media(&mut after);
    save(&client, card_id, &before, &after, &mut log)?;
    let mut out = edit_output(card_id, &after, &what, EditState::Saved);
    out.json["uploaded"] = json!(done.iter().filter(|u| !u.reused).count());
    out.json["reused"] = json!(done.iter().filter(|u| u.reused).count());
    Ok(out)
}

/// Send the changed playlist back under its own card ID, once it is clear
/// the stored playlist is still the `before` the change was made to. An edit
/// can wait minutes on uploads; saving over a newer version would silently
/// undo whatever changed it.
fn save(
    client: &api::Client,
    card_id: &str,
    before: &Value,
    card: &Value,
    log: &mut impl FnMut(String),
) -> Result<()> {
    if edit::editable(&client.get_content(card_id)?)? != *before {
        return Err(Error::Conflict {
            message: format!(
                "playlist {card_id} changed while this edit was being prepared; nothing was saved"
            ),
        });
    }
    log(format!("saving playlist {card_id}"));
    let saved = client.create_content(card)?;
    match saved.get("cardId").and_then(Value::as_str) {
        Some(id) if id == card_id => Ok(()),
        other => Err(Error::Api {
            status: 200,
            message: format!(
                "saving playlist {card_id}: Yoto answered with card {}",
                other.unwrap_or("(none)")
            ),
        }),
    }
}

fn confirmation(card_id: &str, card: &Value, what: &str) -> Error {
    Error::ConfirmationRequired {
        message: format!(
            "this would {what} in playlist {card_id} ({:?}), and cards linked to it play the changed version; check it with --dry-run, then re-run with --yes",
            card["title"].as_str().unwrap_or("")
        ),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditState {
    Unchanged,
    DryRun,
    Saved,
}

/// The playlist details a `content edit` can set, under their Yoto names.
fn details(card: &Value) -> Value {
    let metadata = card.get("metadata");
    Value::Object(
        metadata::FIELDS
            .iter()
            .filter_map(|f| {
                metadata
                    .and_then(|m| m.get(*f))
                    .map(|v| (f.to_string(), v.clone()))
            })
            .collect(),
    )
}

fn edit_output(card_id: &str, card: &Value, what: &str, state: EditState) -> Output {
    let chapters = edit::chapter_summary(card);
    let title = card["title"].as_str().unwrap_or("");
    let json = json!({
        "card_id": card_id,
        "title": title,
        "change": what,
        "dry_run": state == EditState::DryRun,
        "changed": state == EditState::Saved,
        "chapter_count": chapters.len(),
        "track_count": edit::track_count(card),
        "duration_seconds": card.pointer("/metadata/media/duration"),
        "metadata": details(card),
        "chapters": chapters,
    });
    let mut text = match state {
        EditState::Unchanged => format!("No change: {title:?} ({card_id}) already matches.\n"),
        EditState::DryRun => format!("Would {what} in {title:?} ({card_id}).\n"),
        EditState::Saved => format!(
            "Saved {title:?} ({card_id}): {what}. Cards linked to it play the changed version.\n"
        ),
    };
    for ch in &chapters {
        text.push_str(&format!(
            "{:>3}. {}{}\n",
            ch["number"].as_u64().unwrap_or(0),
            ch["title"].as_str().unwrap_or(""),
            match ch["track_count"].as_u64() {
                Some(n) if n > 1 => format!(" ({n} tracks)"),
                _ => String::new(),
            }
        ));
    }
    Output { json, text }
}

pub enum Source<'a> {
    Dir(&'a Path),
    Manifest {
        path: &'a Path,
        book: &'a str,
        playlist: Option<&'a str>,
    },
}

pub struct UploadOptions<'a> {
    pub source: Source<'a>,
    pub title: Option<String>,
    pub cover: Option<PathBuf>,
    pub no_cover: bool,
    /// An icons.json to use instead of the detected one.
    pub icons: Option<PathBuf>,
    /// Icon for every track the icons.json does not name.
    pub icon: Option<String>,
    pub no_icons: bool,
    /// Replace this existing playlist instead of creating a new one.
    pub replace: Option<String>,
    pub metadata: metadata::Metadata,
    pub dry_run: bool,
    pub yes: bool,
    pub transcode_timeout: Duration,
}

/// Build the plan and apply the command-line overrides.
pub fn build_plan(opts: &UploadOptions) -> Result<plan::Plan> {
    let icons_file = match (&opts.icons, opts.no_icons) {
        (_, true) => plan::IconsFile::Ignore,
        (Some(path), false) => plan::IconsFile::Path(path),
        (None, false) => plan::IconsFile::Detect,
    };
    let mut plan = match &opts.source {
        Source::Dir(dir) => plan::from_dir(dir, icons_file)?,
        Source::Manifest {
            path,
            book,
            playlist,
        } => plan::from_manifest(path, book, *playlist, icons_file)?,
    };
    if let Some(t) = &opts.title {
        plan.title = t.trim().to_string();
    }
    if opts.no_cover {
        plan.cover = None;
    } else if let Some(c) = &opts.cover {
        if !c.is_file() {
            return Err(Error::invalid(format!(
                "cover {} does not exist",
                c.display()
            )));
        }
        plan.cover = Some(c.clone());
    }
    if let Some(c) = &plan.cover {
        let head = std::fs::read(c)
            .map_err(|e| Error::invalid(format!("cannot read cover {}: {e}", c.display())))?;
        if plan::cover_content_type(&head).is_none() {
            return Err(Error::invalid(format!(
                "cover {} is neither JPEG nor PNG",
                c.display()
            )));
        }
    }
    if let Some(spec) = &opts.icon {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        plan.icons.default = Some(icons::Icon::parse(spec, &cwd)?);
    }
    plan.check_limits()?;
    opts.metadata.validate()?;
    opts.metadata.apply(&mut serde_json::Map::new())?;
    Ok(plan)
}

/// `upload`: create a playlist from a folder or a manifest playlist.
pub fn upload(ctx: &Context, opts: &UploadOptions, mut log: impl FnMut(String)) -> Result<Output> {
    let plan = build_plan(opts)?;
    let chapters = if plan.chapters.len() == plan.tracks.len() {
        String::new()
    } else {
        format!(" in {} chapters", plan.chapters.len())
    };
    let summary = format!(
        "{:?}: {} tracks{chapters}, {}{}",
        plan.title,
        plan.tracks.len(),
        format_mb(plan.total_bytes()),
        plan.total_seconds()
            .map(|s| format!(", {}", format_duration(s)))
            .unwrap_or_default()
    );
    if !opts.dry_run && !opts.yes {
        let what = match &opts.replace {
            Some(id) => format!("this replaces the content of playlist {id} with"),
            None => "this creates a new playlist on the Yoto account,".into(),
        };
        return Err(Error::ConfirmationRequired {
            message: format!("{what} {summary}; check it with --dry-run, then re-run with --yes"),
        });
    }
    let n = plan.tracks.len();
    let hashes = plan::hash_tracks(&plan, |i, t| {
        log(format!("[{}/{n}] checking {}", i + 1, t.title))
    })?;

    if opts.dry_run {
        let mut json = plan.to_json();
        json["dry_run"] = json!(true);
        json["replace"] = json!(opts.replace);
        json["metadata"] = opts.metadata.to_json();
        let mut text = match &opts.replace {
            Some(id) => format!("Would replace playlist {id} with {summary}\n"),
            None => format!("Would create {summary}\n"),
        };
        if let Some(c) = &plan.cover {
            text.push_str(&format!("Cover: {}\n", c.display()));
        }
        if let Some(src) = &plan.icons.source {
            text.push_str(&format!("Icons: {}\n", src.display()));
        }
        if let Value::Object(fields) = opts.metadata.to_json() {
            for (k, v) in fields {
                text.push_str(&format!(
                    "{k}: {}\n",
                    v.as_str().map_or(v.to_string(), str::to_string)
                ));
            }
        }
        let track_line = |t: &plan::PlannedTrack| {
            format!(
                "{:>8}  {:>8}  {}  [{}{}]",
                t.duration_seconds
                    .map(format_duration)
                    .unwrap_or_else(|| "n/a".into()),
                format_mb(t.bytes),
                t.title,
                t.format.name(),
                t.on_end
                    .map(|e| format!("; then {}", e.name()))
                    .unwrap_or_default(),
            )
        };
        for (i, c) in plan.chapters.iter().enumerate() {
            let icon = plan
                .icons
                .for_chapter(i + 1)
                .map(|ic| ic.describe())
                .unwrap_or_else(|| "default icon".into());
            let icon = icon.rsplit('/').next().unwrap_or(&icon).to_string();
            let tracks = &plan.tracks[c.tracks.clone()];
            if let [t] = tracks {
                text.push_str(&format!("{:>3}. {}  [{icon}]\n", i + 1, track_line(t)));
            } else {
                text.push_str(&format!("{:>3}. {}  [{icon}]\n", i + 1, c.title));
                for (j, t) in tracks.iter().enumerate() {
                    text.push_str(&format!("     {:>2}) {}\n", j + 1, track_line(t)));
                }
            }
        }
        if !plan.ignored.is_empty() {
            text.push_str(&format!("Ignored: {}\n", plan.ignored.join(", ")));
        }
        return Ok(Output { json, text });
    }

    let client = ctx.client()?;
    if let Some(id) = &opts.replace {
        // Fail before uploading anything if the playlist is missing or not ours.
        ensure_mine(&client, id)?;
    }
    let result = upload::run(
        &client,
        &plan,
        &hashes,
        &upload::Options {
            transcode_timeout: opts.transcode_timeout,
            upload_cover: plan.cover.is_some(),
            replace: opts.replace.clone(),
            metadata: opts.metadata.clone(),
        },
        &mut log,
    )?;
    let text = format!(
        "{} {:?} ({}): {} tracks{chapters} ({} uploaded, {} already on Yoto){}.\n{}",
        if opts.replace.is_some() {
            "Replaced"
        } else {
            "Created"
        },
        plan.title,
        result["card_id"].as_str().unwrap_or("?"),
        result["track_count"],
        result["uploaded"],
        result["reused"],
        result["duration_seconds"]
            .as_f64()
            .map(|s| format!(", {}", format_duration(s)))
            .unwrap_or_default(),
        if opts.replace.is_some() {
            "Cards linked to it play the new version."
        } else {
            "Link it to a Make Your Own card in the Yoto app."
        },
    );
    Ok(Output { json: result, text })
}

/// `icons list`: public Yoto icons (or your own uploads), filtered by words.
pub fn icons_list(ctx: &Context, mine: bool, query: &[String]) -> Result<Output> {
    let all = ctx.client()?.display_icons(!mine)?;
    let items: Vec<Value> = all
        .iter()
        .filter(|i| icons::matches(i, query))
        .map(|i| {
            json!({
                "icon": i.get("mediaId").and_then(Value::as_str).map(|id| format!("yoto:#{id}")),
                "title": i.get("title"),
                "tags": i.get("publicTags").cloned().unwrap_or_else(|| json!([])),
                "url": i.get("url").filter(|u| u.is_string()),
            })
        })
        .collect();
    let mut text = String::new();
    if items.is_empty() {
        text.push_str("No matching icons.");
    }
    for item in &items {
        let tags: Vec<&str> = item["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        text.push_str(&format!(
            "{}  {}{}\n",
            item["icon"].as_str().unwrap_or("?"),
            item["title"].as_str().unwrap_or(""),
            if tags.is_empty() {
                String::new()
            } else {
                format!("  ({})", tags.join(", "))
            }
        ));
    }
    Ok(Output {
        json: json!({"items": items, "total": all.len()}),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_format_with_and_without_hours() {
        assert_eq!(format_duration(249.4), "4:09");
        assert_eq!(format_duration(3909.0), "1:05:09");
        assert_eq!(format_duration(0.0), "0:00");
    }
}
