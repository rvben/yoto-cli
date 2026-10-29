//! yoto-cli CLI.
//!
//! Follows The CLI Spec (clispec.dev): text on a TTY, JSON when piped,
//! structured error envelopes on the last line of stderr, a `schema`
//! subcommand, and mutation markers.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::error::ErrorKind as ClapErrorKind;
use clap::{ArgGroup, CommandFactory, Parser, Subcommand, ValueEnum};
use serde_json::json;
use yoto_cli::{
    AppendOptions, Change, Context, Error, LoginOptions, OutputFormat, Source, UploadOptions,
    schema,
};

#[derive(Parser)]
#[command(
    name = "yoto-cli",
    version,
    about = "Manage Yoto Make Your Own playlists from the command line: sign in, upload audio, create and inspect content.",
    long_about = "Manage Yoto Make Your Own playlists from the command line: sign in, upload audio, create and inspect content.\n\nAn independent tool that uses the Yoto Developer API; not made or endorsed by Yoto.\n\nRun `yoto-cli schema` for the machine-readable contract (clispec.dev)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Output format; auto = text on a TTY, JSON when piped.
    #[arg(long, short = 'o', value_enum, default_value = "auto", global = true)]
    output: CliOutput,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in, check or forget the stored Yoto sign-in.
    #[command(subcommand)]
    Auth(AuthCommand),
    /// List, inspect or delete your Make Your Own playlists.
    #[command(subcommand)]
    Content(ContentCommand),
    /// Find icons to show on the player while a track plays.
    #[command(subcommand)]
    Icons(IconsCommand),
    /// Create a playlist from a folder of audio files or a manifest playlist.
    #[command(group(ArgGroup::new("source").required(true).args(["dir", "manifest"])))]
    Upload {
        /// Folder of audio (MP3, M4A/AAC, WAV, FLAC, Ogg/Opus) in name order; each file is a chapter, each subfolder one chapter of its files.
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
        /// A prepared manifest.json; titles, order and cover come from it.
        #[arg(long, requires = "book")]
        manifest: Option<PathBuf>,
        /// Book key in the manifest.
        #[arg(long, requires = "manifest")]
        book: Option<String>,
        /// Playlist name within the book, when it has more than one (e.g. "Kaart 2").
        #[arg(long, requires = "manifest")]
        playlist: Option<String>,
        /// Playlist title (default: the book or folder name).
        #[arg(long)]
        title: Option<String>,
        /// Cover image (JPEG or PNG) instead of the detected one.
        #[arg(long, conflicts_with = "no_cover")]
        cover: Option<PathBuf>,
        /// Do not upload a cover.
        #[arg(long)]
        no_cover: bool,
        /// icons.json assigning icons by chapter number (default: icons.json beside the audio or in the book folder).
        #[arg(long, value_name = "FILE", conflicts_with = "no_icons")]
        icons: Option<PathBuf>,
        /// Icon for every chapter the icons.json does not name: an image file or yoto:#<mediaId>.
        #[arg(long, value_name = "ICON")]
        icon: Option<String>,
        /// Ignore any icons.json.
        #[arg(long)]
        no_icons: bool,
        /// Replace the content of this existing playlist instead of creating a new one.
        #[arg(long, value_name = "CARD_ID")]
        replace: Option<String>,
        #[command(flatten)]
        metadata: Box<MetadataArgs>,
        /// Check everything locally and print the plan; nothing is sent.
        #[arg(long)]
        dry_run: bool,
        /// Confirm creating the playlist.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Seconds to wait for Yoto to transcode each track.
        #[arg(long, default_value_t = 600, value_name = "SECONDS")]
        transcode_timeout: u64,
        /// Tracks uploading or transcoding at once (1–8); 1 runs serially.
        #[arg(long, default_value_t = 3, value_name = "N", value_parser = clap::value_parser!(u8).range(1..=8))]
        concurrency: u8,
    },
    /// Print the machine-readable contract (clispec.dev) as JSON.
    Schema,
    /// Generate a shell completion script.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

/// The playlist details shown in the Yoto app.
#[derive(clap::Args)]
struct MetadataArgs {
    /// Author; repeat for several, the first is shown ("" alone removes them).
    #[arg(long, value_name = "NAME")]
    author: Vec<String>,
    /// Narrator; repeat for several ("" alone removes them).
    #[arg(long, value_name = "NAME")]
    read_by: Vec<String>,
    /// Description ("" removes it).
    #[arg(long, value_name = "TEXT")]
    description: Option<String>,
    /// Category.
    #[arg(long, value_enum)]
    category: Option<yoto_cli::metadata::Category>,
    /// Youngest age the playlist is for, in years (0 to 99).
    #[arg(long, value_name = "YEARS", value_parser = age_parser())]
    min_age: Option<u8>,
    /// Oldest age the playlist is for, in years (0 to 99).
    #[arg(long, value_name = "YEARS", value_parser = age_parser())]
    max_age: Option<u8>,
    /// Language as a BCP 47 tag such as nl or en-GB ("" removes it).
    #[arg(long, value_name = "TAG")]
    language: Option<String>,
}

/// Ages are whole years from 0 to `MAX_AGE`; anything else is a usage error
/// like any other flag value clap cannot accept.
fn age_parser() -> clap::builder::RangedI64ValueParser<u8> {
    clap::value_parser!(u8).range(..=i64::from(yoto_cli::metadata::MAX_AGE))
}

impl From<MetadataArgs> for yoto_cli::metadata::Metadata {
    fn from(a: MetadataArgs) -> Self {
        yoto_cli::metadata::Metadata {
            authors: (!a.author.is_empty()).then_some(a.author),
            narrators: (!a.read_by.is_empty()).then_some(a.read_by),
            description: a.description,
            category: a.category,
            min_age: a.min_age,
            max_age: a.max_age,
            language: a.language,
        }
    }
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Sign in through the browser (OAuth with PKCE, loopback redirect).
    Login {
        /// Public client ID from https://dashboard.yoto.dev/ (or YOTO_CLIENT_ID; remembered after the first login).
        #[arg(long)]
        client_id: Option<String>,
        /// Loopback port; http://127.0.0.1:<port>/callback must be a registered redirect URI.
        #[arg(long, default_value_t = 8787)]
        port: u16,
        /// OAuth scopes to request.
        #[arg(long, default_value = yoto_cli::auth::DEFAULT_SCOPE)]
        scope: String,
        /// Print the sign-in link without opening a browser.
        #[arg(long)]
        no_browser: bool,
        /// Seconds to wait for the browser redirect.
        #[arg(long, default_value_t = 300, value_name = "SECONDS")]
        timeout: u64,
    },
    /// Show the stored sign-in (no network request).
    Status,
    /// Forget the stored tokens.
    Logout,
}

#[derive(Subcommand)]
enum ContentCommand {
    /// List your Make Your Own playlists.
    List,
    /// Show one playlist with its chapters.
    Show {
        /// Card ID, as shown by `content list`.
        card_id: String,
    },
    /// Delete one of your playlists.
    Delete {
        /// Card ID, as shown by `content list`.
        card_id: String,
        /// Confirm the deletion.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Change a playlist's title or details; cards linked to it keep working.
    Edit {
        /// Card ID, as shown by `content list`.
        card_id: String,
        /// New playlist title.
        #[arg(long)]
        title: Option<String>,
        #[command(flatten)]
        metadata: Box<MetadataArgs>,
        #[command(flatten)]
        confirm: ConfirmArgs,
    },
    /// Add a folder's audio as new chapters at the end of a playlist.
    Append {
        /// Card ID, as shown by `content list`.
        card_id: String,
        /// Folder of audio, read as `upload` reads it; each file is a chapter, each subfolder one chapter.
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        /// icons.json to use instead of DIR/icons.json; chapter numbers count the new chapters from 1.
        #[arg(long, value_name = "FILE", conflicts_with = "no_icons")]
        icons: Option<PathBuf>,
        /// Icon for every new chapter the icons.json does not name: yoto:#<mediaId> or an image file.
        #[arg(long, value_name = "ICON", conflicts_with = "no_icons")]
        icon: Option<String>,
        /// Ignore icons.json and give every new chapter the default icon.
        #[arg(long)]
        no_icons: bool,
        #[command(flatten)]
        confirm: ConfirmArgs,
        /// Seconds to wait for Yoto to transcode each track.
        #[arg(long, default_value_t = 600, value_name = "SECONDS")]
        transcode_timeout: u64,
        /// Tracks uploading or transcoding at once (1–8); 1 runs serially.
        #[arg(long, default_value_t = 3, value_name = "N", value_parser = clap::value_parser!(u8).range(1..=8))]
        concurrency: u8,
    },
    /// Rename, delete, move or re-icon chapters of a playlist.
    #[command(subcommand)]
    Chapter(ChapterCommand),
}

#[derive(Subcommand)]
enum ChapterCommand {
    /// Rename a chapter (and its track, when it has one).
    Rename {
        /// Card ID, as shown by `content list`.
        card_id: String,
        /// Chapter number, as shown by `content show`.
        number: usize,
        /// New chapter title.
        title: String,
        #[command(flatten)]
        confirm: ConfirmArgs,
    },
    /// Delete chapters; the rest are renumbered.
    Delete {
        /// Card ID, as shown by `content list`.
        card_id: String,
        /// Chapter numbers, as shown by `content show`.
        #[arg(required = true, num_args = 1..)]
        numbers: Vec<usize>,
        #[command(flatten)]
        confirm: ConfirmArgs,
    },
    /// Move a chapter to another position.
    Move {
        /// Card ID, as shown by `content list`.
        card_id: String,
        /// Chapter number to move.
        from: usize,
        /// Position it moves to.
        to: usize,
        #[command(flatten)]
        confirm: ConfirmArgs,
    },
    /// Set the icon shown while a chapter plays.
    Icon {
        /// Card ID, as shown by `content list`.
        card_id: String,
        /// Chapter number, as shown by `content show`.
        number: usize,
        /// yoto:#<mediaId> (see `icons list`) or a PNG, GIF or JPEG file.
        icon: String,
        #[command(flatten)]
        confirm: ConfirmArgs,
    },
}

/// How a change to a playlist is confirmed.
#[derive(clap::Args)]
struct ConfirmArgs {
    /// Show the change without making it.
    #[arg(long)]
    dry_run: bool,
    /// Confirm the change.
    #[arg(long, short = 'y')]
    yes: bool,
}

impl From<ConfirmArgs> for yoto_cli::Confirm {
    fn from(a: ConfirmArgs) -> Self {
        yoto_cli::Confirm {
            dry_run: a.dry_run,
            yes: a.yes,
        }
    }
}

#[derive(Subcommand)]
enum IconsCommand {
    /// List Yoto's public icons (or your uploads), optionally filtered.
    List {
        /// Keep icons whose title or tags contain every word.
        #[arg(long, short = 'q', value_name = "WORD", num_args = 1..)]
        query: Vec<String>,
        /// Your own uploaded icons instead of the public ones.
        #[arg(long)]
        mine: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum CliOutput {
    Auto,
    Json,
    Text,
}

impl CliOutput {
    fn resolve(self) -> OutputFormat {
        match self {
            CliOutput::Json => OutputFormat::Json,
            CliOutput::Text => OutputFormat::Text,
            CliOutput::Auto => {
                if std::io::stdout().is_terminal() {
                    OutputFormat::Text
                } else {
                    OutputFormat::Json
                }
            }
        }
    }
}

fn progress(line: String) {
    eprintln!("{line}");
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => return handle_clap_error(e),
    };
    let format = cli.output.resolve();

    let result = match cli.command {
        Command::Schema => {
            println!("{}", schema::contract_json());
            return ExitCode::SUCCESS;
        }
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
            return ExitCode::SUCCESS;
        }
        command => Context::from_env().and_then(|ctx| dispatch(&ctx, command)),
    };

    match result {
        Ok(output) => {
            let _ = writeln!(std::io::stdout(), "{}", output.render(format));
            ExitCode::SUCCESS
        }
        Err(err) => {
            emit_error(&err);
            ExitCode::from(err.exit_code() as u8)
        }
    }
}

fn dispatch(ctx: &Context, command: Command) -> yoto_cli::Result<yoto_cli::Output> {
    match command {
        Command::Auth(AuthCommand::Login {
            client_id,
            port,
            scope,
            no_browser,
            timeout,
        }) => yoto_cli::auth_login(
            ctx,
            &LoginOptions {
                client_id,
                port,
                scope,
                open_browser: !no_browser,
                timeout: Duration::from_secs(timeout),
            },
            progress,
        ),
        Command::Auth(AuthCommand::Status) => yoto_cli::auth_status(ctx),
        Command::Auth(AuthCommand::Logout) => yoto_cli::auth_logout(ctx),
        Command::Content(ContentCommand::List) => yoto_cli::content_list(ctx),
        Command::Content(ContentCommand::Show { card_id }) => yoto_cli::content_show(ctx, &card_id),
        Command::Content(ContentCommand::Delete { card_id, yes }) => {
            yoto_cli::content_delete(ctx, &card_id, yes)
        }
        Command::Content(ContentCommand::Edit {
            card_id,
            title,
            metadata,
            confirm,
        }) => {
            let metadata = (*metadata).into();
            yoto_cli::content_edit(
                ctx,
                &card_id,
                &Change::Details {
                    title: title.as_deref(),
                    metadata: &metadata,
                },
                confirm.into(),
                progress,
            )
        }
        Command::Content(ContentCommand::Append {
            card_id,
            dir,
            icons,
            icon,
            no_icons,
            confirm,
            transcode_timeout,
            concurrency,
        }) => yoto_cli::content_append(
            ctx,
            &card_id,
            &AppendOptions {
                dir: &dir,
                icons,
                icon,
                no_icons,
                confirm: confirm.into(),
                transcode_timeout: Duration::from_secs(transcode_timeout),
                concurrency: usize::from(concurrency),
            },
            progress,
        ),
        Command::Content(ContentCommand::Chapter(cmd)) => {
            let (card_id, change, confirm) = match &cmd {
                ChapterCommand::Rename {
                    card_id,
                    number,
                    title,
                    confirm,
                } => (
                    card_id,
                    Change::RenameChapter {
                        number: *number,
                        title,
                    },
                    confirm,
                ),
                ChapterCommand::Delete {
                    card_id,
                    numbers,
                    confirm,
                } => (card_id, Change::DeleteChapters(numbers), confirm),
                ChapterCommand::Move {
                    card_id,
                    from,
                    to,
                    confirm,
                } => (
                    card_id,
                    Change::MoveChapter {
                        from: *from,
                        to: *to,
                    },
                    confirm,
                ),
                ChapterCommand::Icon {
                    card_id,
                    number,
                    icon,
                    confirm,
                } => (
                    card_id,
                    Change::ChapterIcon {
                        number: *number,
                        icon,
                    },
                    confirm,
                ),
            };
            let confirm = yoto_cli::Confirm {
                dry_run: confirm.dry_run,
                yes: confirm.yes,
            };
            yoto_cli::content_edit(ctx, card_id, &change, confirm, progress)
        }
        Command::Icons(IconsCommand::List { query, mine }) => {
            yoto_cli::icons_list(ctx, mine, &query)
        }
        Command::Upload {
            dir,
            manifest,
            book,
            playlist,
            title,
            cover,
            no_cover,
            icons,
            icon,
            no_icons,
            replace,
            metadata,
            dry_run,
            yes,
            transcode_timeout,
            concurrency,
        } => {
            let source = match (&dir, &manifest, &book) {
                (Some(d), None, _) => Source::Dir(d),
                (None, Some(m), Some(b)) => Source::Manifest {
                    path: m,
                    book: b,
                    playlist: playlist.as_deref(),
                },
                _ => {
                    return Err(Error::Usage {
                        message: "give either DIR or --manifest with --book".into(),
                    });
                }
            };
            yoto_cli::upload(
                ctx,
                &UploadOptions {
                    source,
                    title,
                    cover,
                    no_cover,
                    icons,
                    icon,
                    no_icons,
                    replace,
                    metadata: (*metadata).into(),
                    dry_run,
                    yes,
                    transcode_timeout: Duration::from_secs(transcode_timeout),
                    concurrency: usize::from(concurrency),
                },
                progress,
            )
        }
        Command::Schema | Command::Completions { .. } => unreachable!("handled before dispatch"),
    }
}

/// Help and version print normally and exit 0; every other clap failure becomes
/// a structured `usage` error envelope.
fn handle_clap_error(e: clap::Error) -> ExitCode {
    match e.kind() {
        ClapErrorKind::DisplayHelp
        | ClapErrorKind::DisplayVersion
        | ClapErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let _ = e.print();
            ExitCode::SUCCESS
        }
        _ => {
            let err = Error::Usage {
                message: e.to_string().trim().to_string(),
            };
            emit_error(&err);
            ExitCode::from(err.exit_code() as u8)
        }
    }
}

/// Write the clispec error envelope as the last line of stderr.
fn emit_error(err: &Error) {
    let mut error = serde_json::Map::new();
    error.insert("kind".into(), json!(err.kind()));
    error.insert("message".into(), json!(err.to_string()));
    error.insert("exit_code".into(), json!(err.exit_code()));
    if let Some(hint) = err.hint() {
        error.insert("hint".into(), json!(hint));
    }
    eprintln!("{}", json!({ "error": error }));
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// Every leaf subcommand with its arguments as the schema names them:
    /// `--long` for flags, the id for positionals.
    fn clap_commands() -> BTreeSet<(String, BTreeSet<String>)> {
        fn walk(cmd: &clap::Command, prefix: &str, out: &mut BTreeSet<(String, BTreeSet<String>)>) {
            for sub in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
                let name = format!("{prefix}{}", sub.get_name()).trim().to_string();
                if sub.has_subcommands() {
                    walk(sub, &format!("{name} "), out);
                    continue;
                }
                let args = sub
                    .get_arguments()
                    .filter(|a| !a.is_global_set() && a.get_id() != "help")
                    .map(|a| match a.get_long() {
                        Some(long) => format!("--{long}"),
                        None => a.get_id().to_string(),
                    })
                    .collect();
                out.insert((name, args));
            }
        }
        let mut out = BTreeSet::new();
        walk(&Cli::command(), "", &mut out);
        out
    }

    #[test]
    fn the_schema_lists_exactly_the_commands_and_arguments_clap_accepts() {
        let contract = schema::contract();
        let declared: BTreeSet<(String, BTreeSet<String>)> = contract["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let args = c["args"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|a| a["name"].as_str().unwrap().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                (c["name"].as_str().unwrap().to_string(), args)
            })
            .collect();
        assert_eq!(clap_commands(), declared);
    }
}
