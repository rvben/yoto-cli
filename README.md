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
cargo install yoto-cli
```

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

Point `upload` at a folder of MP3 or M4A files. Tracks are taken in file-name
order and a numeric prefix such as `01 - ` is dropped from the title. A
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

Each track becomes its own chapter, so the skip buttons on the player move
between them. Linking the playlist to a physical card is done in the Yoto app.

To change a playlist later (new art, a corrected title) without breaking the
card linked to it, replace it in place:

```sh
yoto-cli content list
yoto-cli upload --yes --replace <CARD_ID> "Boer Boris/Kaart"
```

### Chapter icons

The player shows a 16x16 pixel icon while a chapter plays. Put an `icons.json`
beside the audio (or in the book folder) mapping track numbers to icons:

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
resized by Yoto. Tracks without an entry get `"default"`, or Yoto's standard
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
