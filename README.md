# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Plasmite is a **Unix-first CLI** for working with **Plasma-style pools**: a single-file,
mmap-backed ring buffer of immutable messages that multiple local processes can append to and
read from concurrently.

It’s inspired by Oblong’s Plasma / libPlasma pool model (pools, descrips, rewindable streams),
but focuses on a modern, script-stable CLI and a tight local storage story.

## What you get

- **Single-file pools**: each pool is a `.plasmite` file (no daemon required).
- **Room-scale concurrency**: many readers + many writers (writers serialize at append time).
- **JSON-first UX**: JSON by default; streaming commands emit JSONL when piped.
- **Stable automation**: predictable exit codes and a versioned message schema.
- **Zero-copy storage format**: payloads are stored as **Lite³** documents (see “Why Lite³”).

Supported platforms: **macOS** and **Linux**.

## Status / CLI surface (v0.0.1 contract)

The intentionally small CLI for the initial release is:

- `plasmite pool create`
- `plasmite pool info`
- `plasmite pool bounds`
- `plasmite poke`
- `plasmite get`
- `plasmite peek`
- `plasmite bench`
- `plasmite version`

The contract is documented in `plasmite-cli-spec-v.0.1.md`.

v0.0.1 focuses on the CLI only; a dedicated Rust library API will come later.

## Install

### From source (recommended while early)

```bash
cargo install --path . --locked
```

Tip: a short alias binary, `pls`, is installed alongside `plasmite` when available.

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
plasmite --dir .scratch/pools pool create demo

# append a message (repeat --descrip to add more tags)
plasmite --dir .scratch/pools poke demo --print --descrip ping --data-json '{"x":1}'

# fetch by seq (replace <seq> with printed seq)
plasmite --dir .scratch/pools get demo <seq>

# peek the last message
plasmite --dir .scratch/pools peek demo --tail 1

# follow (Ctrl-C to stop)
plasmite --dir .scratch/pools peek demo --follow
```

Tip: `peek` is designed to compose with Unix tools:

```bash
plasmite --dir .scratch/pools peek demo --follow | jq -c '.data'
```

### Two-terminal live stream demo (macOS logs)

This shows a real, high-frequency data source (macOS unified logging) being streamed into a pool
in one terminal while another terminal follows it.

Terminal 1 (writer):

```bash
plasmite pool create demo

/usr/bin/log stream --style ndjson --level info \
  | plasmite poke demo --descrip log
```

Terminal 2 (reader):

```bash
plasmite peek demo --follow --jsonl
```

Notes:
- Use `/usr/bin/log` (in zsh, `log` can be a shell builtin).
- Prefer `--style ndjson` for streaming; `--style json` is one big JSON value, so `jq` may appear to “hang”.
- `poke` is silent by default; add `--print` if you want append acks.
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
  "pool": "demo",
  "seq": 12345,
  "ts": "2026-01-28T18:06:00.123Z",
  "meta": { "descrips": ["event", "ping"] },
  "data": { "any": "thing" }
}
```

On disk, each message payload is stored as **Lite³ bytes for `{meta,data}`**; the CLI remains
**JSON-in/JSON-out** (`poke` encodes JSON to Lite³; `get`/`peek` decode Lite³ to JSON. Use `poke --print` to emit JSON acks).

Errors:
- On TTY, errors are concise human text (one line + hint).
- When stderr is not a TTY (piped/redirected), errors are JSON objects on stderr for easy parsing.

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

- CLI contract: `plasmite-cli-spec-v.0.1.md`
- Storage + concurrency design: `plasmite-tdd-v.0.0.1.md`
- Testing: `TESTING.md`
- Release checklist: `RELEASING.md`
- Exit codes: `docs/exit-codes.md`
- Homebrew packaging: `docs/homebrew.md`
- Performance baselines: `docs/perf.md`
- Changelog: `CHANGELOG.md`

## Development

```bash
cargo test
```

See `TESTING.md` for the full list of test suites and what they cover.

## Third-party / licenses

See `THIRD_PARTY_NOTICES.md` for vendored dependencies and licenses.
