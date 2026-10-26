# yoto-cli

[![CI](https://github.com/rvben/yoto-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/rvben/yoto-cli/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/yoto-cli.svg)](https://crates.io/crates/yoto-cli)
[![clispec compliant](https://img.shields.io/badge/clispec-compliant-3b82f6)](https://clispec.dev)

Manage Yoto Make Your Own playlists from the command line: sign in, upload
audio with a cover and chapter icons, and list, inspect, replace or delete
your playlists.

yoto-cli is an independent tool built on the public
[Yoto Developer API](https://yoto.dev). It is not made or endorsed by Yoto.

## Install

```sh
brew install rvben/tap/yoto-cli   # macOS, Linux
uv tool install yoto-cli          # or: pipx install yoto-cli
cargo install yoto-cli
```

Prebuilt binaries for macOS, Linux and Windows, each with a SHA-256 checksum,
are attached to every [GitHub release](https://github.com/rvben/yoto-cli/releases).

## Sign in

The Yoto API needs a client ID of your own:

1. Create a **public** client at <https://dashboard.yoto.dev/>.
2. Add `http://127.0.0.1:8787/callback` as an allowed callback URL.
3. Under Scopes, tick `user:content:manage` (which brings in `user:content:view`
   and `user:icons:manage`) and `offline_access`. Without `offline_access` the
   sign-in page fails with "scopes that have not been pre-approved".
4. Sign in once:

```sh
yoto-cli auth login --client-id <CLIENT_ID>
```

The browser opens the Yoto sign-in page (OAuth with PKCE; no client secret is
involved). The tokens go into the OS keychain (the macOS Keychain, the Windows
Credential Manager, or the Secret Service on Linux) and the client ID into the
config file, so later commands need neither. On a host without one, such as a
headless Linux server, set `YOTO_CLI_TOKEN_FILE` instead. Access tokens are
refreshed automatically, also in the middle of a long upload;
`yoto-cli auth status` shows the state without a network request, and
`yoto-cli auth logout` forgets the tokens.

## Upload a playlist

Point `upload` at a folder of audio files: MP3, M4A/AAC, WAV, FLAC or Ogg
(Opus or Vorbis), recognised by their content rather than their extension.
AIFF is refused up front, because Yoto cannot transcode it. Tracks are taken in
file-name order, with numbers compared by value (`2` before `10`) and without
regard to case, and a numeric prefix such as `01 - ` is dropped from the title. Each subfolder becomes one chapter of the
audio in it; folders nested deeper are listed as ignored in the dry run. A
`cover.jpg` or `cover.png` in the folder (or its parent) becomes the cover;
`--cover FILE` picks another and `--no-cover` sends none. The title defaults to
the folder name (`--title` overrides it).

```sh
yoto-cli upload --dry-run "Boer Boris/Kaart"   # check everything, send nothing
yoto-cli upload --yes "Boer Boris/Kaart"
```

Before anything is sent, the Yoto limits are checked: at most 100 tracks, 100 MB
and 60 minutes per track, and 500 MB per playlist. Yoto keeps files by SHA-256,
so a file it already has is not uploaded again and an interrupted run can
simply be repeated.

Each file becomes its own chapter, so the skip buttons on the player move
between them. A subfolder becomes one chapter of the files in it, played in
order, which suits a song in parts or a story split into several files:

```
Liedjes/
  01 - Intro.mp3          chapter 1
  02 - Vol. 2/            chapter 2 "Vol. 2"
    01 - Eerste.mp3
    02 - Tweede.mp3
  03 - Slot.mp3           chapter 3
```

Linking the playlist to a physical card is done in the Yoto app.

The details shown in the Yoto app can be set at the same time:

```sh
yoto-cli upload --yes "Boer Boris/Kaart" \
  --author "Ted van Lieshout" --read-by "Ruben" \
  --description "Boer Boris en zijn dieren" \
  --category stories --min-age 2 --max-age 6 --language nl
```

Repeat `--author` or `--read-by` for several people, in order; the app shows
the first author. A single `""` removes them all.

To change a playlist later (new art, a corrected title) without breaking the
card linked to it, replace it in place:

```sh
yoto-cli content list
yoto-cli upload --yes --replace <CARD_ID> "Boer Boris/Kaart"
```

### Chapter icons

The player shows a 16x16 pixel icon while a chapter plays. Put an `icons.json`
beside the audio (or in the book folder) mapping chapter numbers to icons
(without subfolders, chapter numbers are track numbers):

```json
{
  "default": "yoto:#aUm9i3ex3qqAMYBv-i-O-pYMKuMJGICtR3Vhf289u2Q",
  "1": "icons/tractor.png",
  "2": "yoto:#7Wt--qhx9t7PCSC8WBmGKJvlG53Z1uk7bN1dE9wGWJ0"
}
```

A value is either an image path relative to the JSON file, uploaded as your own
icon, or a `yoto:#<mediaId>` reference to an icon Yoto already has. A 16x16 RGBA
PNG or a 16x16 GIF is kept exactly as drawn; any other PNG, GIF or JPEG is
resized by Yoto. Chapters without an entry get `"default"`, or Yoto's standard
icon when there is none.

Find public icons by title or tag:

```sh
yoto-cli icons list -q tractor
yoto-cli icons list --mine
```

`--icons FILE` uses another icons.json, `--icon ICON` sets the default from the
command line, and `--no-icons` ignores any icons.json.

### From a manifest

A prepared `manifest.json` can supply titles, order, cover and the expected
SHA-256 of every file:

```sh
yoto-cli upload --dry-run --manifest manifest.json --book boerboris
```

An entry may also name a `chapter`: consecutive entries with the same chapter
play as one chapter. `"on_end": "stop"` or `"repeat"` sets what the player does
when that track ends, such as stopping after a bedtime story or repeating a
lullaby:

```json
{"file": "Kaart/05.mp3", "title": "Slaapliedje", "bytes": 1234567,
 "sha256": "...", "chapter": "Welterusten", "on_end": "repeat"}
```

## Edit a playlist

Edits fetch the playlist, change it and save it under the same card ID, so
cards linked to it keep working. Every edit needs `--yes`; `--dry-run` shows
the result first.

```sh
yoto-cli content edit <CARD_ID> --title "Boer Boris" --author "Ted van Lieshout" --yes
yoto-cli content append <CARD_ID> ./Boer\ Boris/Kaart\ 2 --yes   # new chapters at the end
yoto-cli content chapter rename <CARD_ID> 3 "Het einde" --yes
yoto-cli content chapter move <CARD_ID> 5 1 --yes                 # chapter 5 becomes chapter 1
yoto-cli content chapter delete <CARD_ID> 4 6 --yes
yoto-cli content chapter icon <CARD_ID> 2 icons/koe.png --yes     # or yoto:#<mediaId>
```

Chapter numbers are the ones `content show` prints. Chapters keep their
internal keys when they move, and the number shown on the player follows the
new position. `append` reads a folder the way `upload` does and checks the
100-track limit including the tracks already in the playlist. Only an
`icons.json` in the appended folder itself is used, numbering the new chapters
from 1; one in the book folder belongs to the book's own chapters.

## Other commands

```sh
yoto-cli content list                 # your playlists
yoto-cli content show <CARD_ID>       # chapters and durations
yoto-cli content delete <CARD_ID> --yes
yoto-cli completions zsh > _yoto-cli
```

## Environment

| variable | effect |
| --- | --- |
| `YOTO_CLIENT_ID` | client ID for `auth login` when `--client-id` is not given |
| `YOTO_CLI_TOKEN_FILE` | keep tokens in this file (mode 0600) instead of the OS keychain, e.g. on a headless host |
| `YOTO_CLI_CONFIG_DIR` | directory of the config file |
| `YOTO_CLI_API_URL`, `YOTO_CLI_AUTH_URL` | other API and sign-in base URLs |

## Exit codes

| code | kind | meaning |
| --- | --- | --- |
| `0` | | success |
| `1` | `invalid_input` | a local file, folder, manifest, cover or icon is unusable |
| `3` | `usage` | invalid arguments or a missing client ID |
| `4` | `not_authenticated` | not signed in, or the sign-in can no longer be refreshed |
| `5` | `confirmation_required` | a mutating command needs `--yes`; nothing changed |
| `6` | `limit_exceeded` | the playlist breaks a Yoto limit |
| `7` | `not_found` | the playlist does not exist or is not yours |
| `8` | `api_error` | the Yoto API returned an error |
| `9` | `network` | no HTTP answer (retryable) |
| `10` | `transcode_timeout` | Yoto did not finish transcoding in time (retryable) |
| `11` | `auth_failed` | the browser sign-in did not complete |
| `12` | `token_store` | the keychain, token file or config could not be read or written |
| `13` | `conflict` | the playlist changed while an edit was being prepared; nothing was saved (retryable) |
| `14` | `transcode_failed` | Yoto could not transcode a track (it names the file and the reason); nothing was saved |

## For agents (clispec)

yoto-cli follows [The CLI Spec](https://clispec.dev): text on a TTY and JSON
when piped (`-o json` / `-o text` to choose), progress on stderr, a structured
error envelope on the last line of stderr, and a `schema` subcommand whose
output validates against `clispec.dev/schema/v0.3.json` (checked by the test
suite).

```sh
yoto-cli schema
```

## License

MIT
