# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Plasmite is a command line tool (and eventually library) for reading and writing messages into **pools**. Pools are persistent ring buffers of immutable messages that multiple local processes can append to and read from concurrently. They're a handy abstraction for coordinating small distributed systems such as spatial computing environments, installations, robotics setups, music & performances, etc. 

Pools and their messages can play a similar role to [OSC](https://ccrma.stanford.edu/groups/osc/index.html), but plasma is more approachable and its data is just JSON.

It’s inspired by Oblong Industries' [Plasma](https://github.com/plasma-hamper/plasma) but simplified, written in Rust, and with JSON semantics. 

## Basics

- **Single-file pools**: each pool is a `.plasmite` file
- **Room-scale concurrency**: many readers + many writers (writers serialize at append time).
- **JSON contract**: JSON in and out
- **Zero-copy storage format**: payloads are stored as **[Lite³](https://github.com/fastserial/lite3)** messages (see “Why Lite³”).

Supported platforms: **macOS** and **Linux**.

## Interface

- `plasmite pool create` -- make a pool file
- `plasmite pool info` -- info about the pool
- `plasmite pool list` -- list pools in the pool dir
- `plasmite pool delete` -- delete a pool file
- `plasmite poke` -- deposit messages into a pool
- `plasmite get` -- read one message from a pool
- `plasmite peek` -- stream all messages from a pool

An alias binary, `pls`, is installed as well.

Library will come later.

## Install

### From source

```bash
cargo install --path . --locked
```

### Homebrew (tap)

Copy `homebrew/plasmite.rb` into your tap repository.
See `docs/homebrew.md` for the full steps.

```bash
brew tap YOUR_GITHUB/tap
brew install plasmite
```

## Quickstart

```bash
# create a pool
plasmite pool create foo

# append a message (repeat --descrip to add more tags)
plasmite poke foo --descrip greeting '{"hello":"world"}'

# fetch by sequence
plasmite get foo 1

# watch for new messages (Ctrl-C to stop)
plasmite peek foo

# watch messages from the last 5 minutes
plasmite peek foo --since 5m
```

Tip: `peek` is designed to compose with Unix tools. Use `--where` to filter at the source,
then use `jq` to shape output:

```bash
plasmite peek foo --where '.meta.descrips[]? == "greeting"' --format jsonl | jq -r '.data.hello'
```

## Help

```bash
plasmite --help
plasmite peek --help
plasmite pool create --help
```

### Streaming into `poke`

`poke` is designed to accept common streaming formats with no glue:

```bash
# create a pool for the stream (or use --create on poke)
plasmite pool create ingest

# JSON Lines (jq-friendly)
jq -c '.items[]' data.json | plasmite poke ingest

# Event-style streams (lines prefixed with data:)
curl -N https://example.com/stream | plasmite poke ingest

# JSON Sequence (0x1e record separators, common on Linux)
journalctl -o json-seq -f | plasmite poke ingest
```

Use `--in` to force a mode when auto-detection is wrong:

```bash
cat payload.json | plasmite poke ingest --in json
journalctl -o json-seq -f | plasmite poke ingest --in seq
```

Use `--errors skip` for best-effort ingestion; skipped records emit notices on stderr
and set exit code 1. In auto mode, multiline recovery resyncs on lines that look like
the start of a new JSON value (`{` or `[`); for strict multiline JSON, use `--in json`.
Append/storage errors are not skippable and will abort ingestion.

### Two-terminal live stream demo (macOS logs)

This shows a real, high-frequency data source (macOS unified logging) being streamed into a pool
in one terminal while another terminal follows it.

Terminal 1 (writer):

```bash
plasmite pool create logs

/usr/bin/log stream --style ndjson --level info \
  | plasmite poke logs --descrip log
```

Terminal 2 (reader):

```bash
plasmite peek logs
```

Notes:
- Use `/usr/bin/log` (in zsh, `log` can be a shell builtin).
- Prefer `--style ndjson` for streaming; `--style json` is one big JSON value, so `jq` may appear to “hang”.
- `poke` emits the committed message as JSON.
- If it’s too chatty, add a filter (but avoid filters so strict that nothing matches):
  `--predicate 'subsystem == "com.apple.SkyLight"'` or `--process WindowServer`.

## Performance baselines

Use a release build for baseline numbers. See `docs/perf.md` for the full guide.

## Concepts

### Pools

A “pool” is a **persistent ring buffer** of messages:

- Appends increase a monotonically increasing `seq` number.
- Once the pool is full, newer appends overwrite the oldest messages (ring-buffer semantics).
- Readers are best-effort: if writers outrun readers, readers can observe gaps (via `seq`).

### Messages

Plasmite’s user-facing message model is a simplification of Plasma “proteins”:

- `meta.descrips`: a list of strings (like Plasma “descrips”) for lightweight tagging/filtering.
- `data`: arbitrary JSON object (your payload).

Canonical CLI JSON shape:

```json
{
  "seq": 12345,
  "time": "2026-01-28T18:06:00.123Z",
  "meta": { "descrips": ["event", "ping"] },
  "data": { "any": "thing" }
}
```

On disk, each message payload is stored as **Lite³ bytes for `{meta,data}`**; the CLI remains
**JSON-in/JSON-out** (`poke` encodes JSON to Lite³; `get`/`peek` decode Lite³ to JSON).

Errors:
- On TTY, errors are concise human text (one line + hint).
- When stderr is not a TTY (piped/redirected), errors are JSON objects on stderr for easy parsing.
- On TTY, pretty JSON stdout is colorized by default; disable with `--color never`.

Example (non-TTY JSON error):
```json
{"error":{"kind":"NotFound","message":"pool not found","path":"/Users/me/.plasmite/pools/demo.plasmite","hint":"Create it first: plasmite pool create demo (or pass --dir for a different pool directory)."}}
```

Example (TTY text error):
```
error: invalid duration
hint: Use a number plus ms|s|m|h (e.g. 10s).
```

### Pool references

Most commands take a pool reference:

- If the argument contains `/`, it’s treated as a path.
- If it ends with `.plasmite`, it resolves to `POOL_DIR/<arg>`.
- Otherwise it resolves to `POOL_DIR/<NAME>.plasmite`.

Default pool dir is `~/.plasmite/pools` (override with `--dir`).

## Why Lite³

Plasmite uses **Lite³** (a JSON-compatible, zero-copy binary format) for message payloads:

- **Zero-copy reads**: the pool is memory-mapped; readers can validate and view payload bytes
  without a full JSON parse/deserialize step.
- **Small + fast**: Lite³ is designed to make “wire format == memory format” practical.
- **Debuggable**: Lite³ supports conversion to/from JSON, so the CLI can stay JSON-native.

Links:
- Lite³ docs: `https://lite3.io/`
- Lite³ repo: `https://github.com/fastserial/lite3`

This repo vendors a pinned Lite³ snapshot in `vendor/lite3/` to keep builds reproducible and
to avoid network fetches at build time. See `vendor/README.md`.

## Docs

- CLI contract: `spec/v0/SPEC.md`
- Storage + concurrency design: `docs/design/tdd-v0.0.1.md`
- Vision: `docs/vision.md`
- Architecture: `docs/architecture.md`
- Roadmap: `docs/roadmap.md`
- Decisions (ADRs): `docs/decisions/README.md`
- Docs index: `docs/README.md`
- Testing: `docs/TESTING.md`
- Release checklist: `docs/RELEASING.md`
- Exit codes: `docs/exit-codes.md`
- Homebrew packaging: `docs/homebrew.md`
- Performance baselines: `docs/perf.md`
- Changelog: `CHANGELOG.md`

## Development

```bash
cargo test
```

See `docs/TESTING.md` for the full list of test suites and what they cover.

## Third-party / licenses

See `THIRD_PARTY_NOTICES.md` for vendored dependencies and licenses.
