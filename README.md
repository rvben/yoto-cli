# yoto-cli

[![CI](https://github.com/rvben/yoto-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/rvben/yoto-cli/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/yoto-cli.svg)](https://crates.io/crates/yoto-cli)
[![clispec](https://img.shields.io/badge/clispec-v0.2-blue)](https://clispec.dev)

Manage Yoto Make Your Own playlists from the command line: sign in, upload audio, create and inspect content.

## Install

```sh
cargo install yoto-cli
```

## Usage

```sh
yoto-cli 21          # => 21 doubled is 42   (text on a TTY)
yoto-cli 21 | jq .   # => {"value":21,"doubled":42}   (JSON when piped)
```

> This is the scaffolded example command. Replace the `run` logic in
> `src/lib.rs`, the command in `src/schema.rs`, and these docs with your tool.

## Exit codes

| code | meaning |
| --- | --- |
| `0` | success |
| `1` | invalid input |
| `3` | usage error |

## For agents (clispec)

yoto-cli follows [The CLI Spec](https://clispec.dev): structured output on
stdout, structured error envelopes on the last line of stderr, and a `schema`
subcommand whose output validates against `clispec.dev/schema/v0.2.json`
(checked by the test suite).

```sh
yoto-cli schema
```

## License

MIT
