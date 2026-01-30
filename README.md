# Plasmite

Plasmite is a **Unix-first CLI + Rust library** for working with **Plasma-style pools**: a
single-file, mmap-backed ring buffer of immutable messages that multiple local processes can
append to and read from concurrently.

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
- `plasmite version`

The contract is documented in `plasmite-cli-spec-v.0.1.md`.

## Install

### From source (recommended while early)

```bash
cargo install --path .
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
plasmite --dir .scratch/pools pool create demo

# append a message (repeat --descrip to add more tags)
plasmite --dir .scratch/pools poke demo --descrip ping --data-json '{"x":1}'

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
**JSON-in/JSON-out** (`poke` encodes JSON to Lite³; `get`/`peek` decode Lite³ to JSON).

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
- Storage + concurrency design: `plasmite-tdd-v.0.1.md`
- Exit codes: `docs/exit-codes.md`
- Homebrew packaging: `docs/homebrew.md`

## Development

```bash
cargo test
```

## Third-party / licenses

See `THIRD_PARTY_NOTICES.md` for vendored dependencies and licenses.
