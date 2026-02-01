# Plasmite Spec (v0)

> Normative v0 contract. Formerly `plasmite-cli-spec-v.0.1.md`.

## Design goals

* **Unix-first**: everything works via stdin/stdout; streaming is first-class; easy to pipe through `jq`, `rg`, etc.
* **Inspectable**: everything is JSON by default (JSONL for streams).
* **Consistent**: one primary binary with subcommands; consistent flags; good help; shell completion.
* **Script-stable**: stable exit codes; machine formats (JSON, JSONL) that don’t drift.
* **Room-scale**: optimized for “a handful to dozens” of local processes and optionally a small LAN.

---

## v0.0.1 contract (normative)

This section defines the **frozen** CLI surface area for v0.0.1. Everything else in this document is
non-binding until promoted into this section.

### Commands in scope

Only these commands are in-scope for v0.0.1:

* `plasmite pool create`
* `plasmite pool info`
* `plasmite pool delete`
* `plasmite poke`
* `plasmite get`
* `plasmite peek`
* `plasmite version`

### Flags in scope

Minimal, explicit flag set for v0.0.1:

* Global: `--dir`
* `pool create`: `--size`
* `poke`: `DATA`, `--file FILE` (plus stdin stream fallback), `--descrip`, `--durability fast|flush`, `--create`, `--create-size`
* `peek`: `--tail`, `--jsonl`

JSON output is the default for commands that print; `poke` always emits committed message JSON.

Errors:
- On TTY, emit concise human text (single summary line + hint).
- When stderr is not a TTY (piped/redirected), emit a JSON object to stderr:
  - `error.kind` (string)
  - `error.message` (short summary)
  - `error.hint` (optional)
  - `error.path` / `error.seq` / `error.offset` (optional)
  - `error.causes` (optional array; omitted if empty)
  - Omit fields when unknown.

### Output formats

* Non-streaming: JSON only (when output is enabled).
* Streaming: default is pretty JSON per message; `--jsonl` emits one object per line.
* Errors are JSON objects on stderr when stderr is not a TTY; otherwise concise text is used.
* Exit codes are stable and match the core error kinds (see `docs/exit-codes.md`).

### Message schema (fixed)

The CLI message schema is fixed and versioned by convention:

```json
{
  "seq": 12345,
  "time": "2026-01-28T18:06:00.123Z",
  "meta": {
    "descrips": ["event", "ping"]
  },
  "data": { "any": "thing" }
}
```

* `meta.descrips` is always present (empty array if unset).
* On disk: the payload is Lite³ bytes for `{meta,data}`.
* On the CLI: JSON in/out only (encode on `poke`, decode on reads).

### Pool references (normative)

* `NAME` resolves to `POOL_DIR/NAME.plasmite`.
* Explicit paths (`./foo.plasmite`, `/abs/foo.plasmite`) are used as-is.

### Platforms

Supported in v0.0.1: **macOS** and **Linux**.

### Out of scope

* Remote refs (`tcp(s)://...`) and `plasmite serve`.
* Any additional subcommands or flags not listed above.

---

## Naming & compatibility

### Primary binary

* `plasmite`

## Global conventions

### Pool directory & config

* Default pool directory: `~/.plasmite/pools`
* Config file (optional): `~/.plasmite/config.toml`
* Override per command: `--dir PATH`

In this spec, `POOL_DIR` means the effective pool directory after applying `--dir` (or the default if unset).

### Pool references

Accepted everywhere:

* Local pool: `NAME` (resolved in pool dir)
* Explicit path: `./foo.plasmite` or `/abs/path/foo.plasmite`

No “trailing slash required” anywhere.

#### Strong resolution rule (v0.1)

To keep scripting stable and avoid ambiguity:

* If the argument contains a path separator (`/`) → treat it as a path.
* Else if it ends with `.plasmite` → resolve to `POOL_DIR/<arg>`.
* Else → resolve to `POOL_DIR/<NAME>.plasmite`.

### Output formats

Streaming flags:

* `--jsonl` : JSON Lines (one JSON object per message)
* Default for streaming: pretty JSON per message unless `--jsonl` is specified.

All pool/message commands emit JSON by default. The `completion` subcommand emits shell completion scripts (not JSON).

### Color

* On by default for TTY.
* `--no-color` disables.

### Sizes

Accept:

* `64K`, `256M`, `2G` (K/M/G are 1024-based)
* raw bytes: `1048576`

### Time/ranges (for peek)

* `--tail N` (last N currently available)

### Exit codes

* `0` success
* `1` unexpected/internal error (bug)
* `2` usage error (bad args/flags)
* `3` not found (pool missing / message missing)
* `4` already exists
* `5` busy/in use
* `6` permission/auth error
* `7` invalid/corrupt pool
* `8` I/O or network error
* `130` terminated by Ctrl-C

---

## Message model

A “message” is conceptually:

* **seq**: monotonically increasing integer within a pool (primary handle)
* **time**: timestamp (stored as ns; rendered as RFC3339 in CLI JSON)
* **meta**: lightweight metadata (required; at minimum `descrips: string[]`)
* **data**: arbitrary JSON object (primary user payload)

`meta.descrips` is always present (empty array if unset).

Canonical JSON rendering for tools:

```json
{
  "seq": 12345,
  "time": "2026-01-28T18:06:00.123Z",
  "meta": {
    "descrips": ["event", "ping"]
  },
  "data": { "any": "thing" }
}
```

This makes `jq '.meta.descrips[]?'` or `jq '.data'` straightforward.

On disk, each message payload is stored as a **Lite³ document** (bytes). The CLI remains **JSON-in/JSON-out**: `poke` encodes JSON into Lite³, and read/stream commands decode Lite³ back to JSON for printing.

In v0.1, `pool` is the pool reference (either a name like `example` or an explicit path like `/abs/example.plasmite`).

---

# Commands

Top-level groups:

* `plasmite pool …` (pool lifecycle + info)
* `plasmite peek …` (stream/read)
* `plasmite poke …` (append/write)
* `plasmite get …` (random access by seq)
* `plasmite export …` (bulk extract a range)
* `plasmite msg …` (encode/decode/inspect message files)
* `plasmite completion …`, `plasmite doctor`, `plasmite version`

---

## `plasmite pool create`

Create one or more pools.

**Synopsis**

```bash
plasmite pool create [OPTIONS] NAME [NAME...]
```

**Options**

* `--size SIZE` (default: `1M`)
* `--force` : if exists, delete and recreate (legacy `-z`)
* `--if-missing` : don’t error if exists (legacy `-q`)
* `--checksum` : enable per-message checksum/CRC (debuggability; default: off)
* `--dir PATH` (override pool directory)

**Behavior**

* If both `--force` and `--if-missing` are specified, `--force` wins.
* Creates “single-file pools” by default (no pool format variants exposed).
* Under the default resolution rule, `plasmite pool create NAME` creates `POOL_DIR/NAME.plasmite`.
* Pool file permissions are determined by the pool directory + process umask (use `--dir` to target a shared location if desired).

**Output**

* Default: JSON objects to stdout (pretty if TTY, compact otherwise).

---

## `plasmite pool rm`

Delete pools.

**Synopsis**

```bash
plasmite pool rm [OPTIONS] NAME [NAME...]
```

**Options**

* `--if-missing` : no error if missing
* `--glob` : interpret NAME(s) as glob patterns (local only by default)
* `--dir PATH`

**Output**

* Default: JSON objects to stdout (pretty if TTY, compact otherwise).

---

## `plasmite pool mv`

Rename a pool.

**Synopsis**

```bash
plasmite pool mv OLD NEW
```

Rules:

* Local pools only (for v0.1). For remote, prefer “copy/export + import” later.

---

## `plasmite pool resize`

Resize a pool (if supported by the format you choose).

**Synopsis**

```bash
plasmite pool resize NAME SIZE
```

**Output**

* Default: JSON object to stdout (pretty if TTY, compact otherwise).

---

## `plasmite pool ls`

List pools.

**Synopsis**

```bash
plasmite pool ls [OPTIONS]
```

**Options**

* `--long` : include size, used, message count, oldest/newest seq, flags
* `--dir PATH`

**Defaults**

* Always JSON to stdout; pretty if TTY, compact otherwise.

---

## `plasmite pool info`

Detailed info for one pool.

**Synopsis**

```bash
plasmite pool info NAME
```

**Output**

* Default: JSON object to stdout (pretty if TTY, compact otherwise).

---

## `plasmite pool delete`

Delete a pool file (destructive).

**Synopsis**

```bash
plasmite pool delete NAME
```

**Behavior**

* Removes the resolved pool file.
* Returns NotFound if the pool does not exist.

---

## `plasmite peek`

Stream messages from a pool.

**Synopsis**

```bash
plasmite peek POOLREF [OPTIONS]
```

**Options**

* `--tail N` (or `-n N`): print the last N messages first, then keep watching.
* `--jsonl`: emit one JSON object per line (recommended for pipes).

**Behavior**

* Without `--tail`, starts at **newest+1** and waits for new messages.
* With `--tail N`, prints the last N messages currently available, then waits for new ones.

---

## `plasmite poke`

Append a message to a pool.

**Synopsis**

```bash
plasmite poke POOLREF [DATA] [OPTIONS]
```

**Options (metadata)**

* `--descrip TEXT` (repeatable)
  * populates `meta.descrips` in-order
  * `meta.descrips` is always present (empty array if no `--descrip` flags)

**Options (data)**

* `DATA` (inline JSON value)
* `--file FILE.json` (read JSON from file; use `-` for stdin)
* If stdin is not a TTY and no `DATA`/`--file` is provided: treat stdin as a stream of JSON values (jq-style). Each JSON value becomes one message.

**Options (durability)**

* `--durability fast|flush` (default: `fast`)
  * `fast`: best-effort (no explicit flush)
  * `flush`: flush frame + header to storage after append
**Options (create)**

* `--create` : create the pool if it is missing
* `--create-size SIZE` : pool size for creation (bytes or K/M/G)

This gives you the classic pattern:

```bash
seq=$(plasmite poke mypool '{"x":1}' --descrip event | jq -r '.seq')
plasmite get mypool "$seq" | jq .
```

---

## `plasmite get`

Fetch one message by seq.

**Synopsis**

```bash
plasmite get POOLREF SEQ [OPTIONS]
```

**Options**

* `--pretty` (force pretty JSON)
* `--data-only` / `--meta-only`
* `--out FILE` : write binary message frame or canonical JSON (paired with `--format`)
* `--format json|bin` (default depends on `--out`)

This replaces `p-nth` with a clearer primitive.

---

## `plasmite export`

Export a range of messages.

**Synopsis**

```bash
plasmite export POOLREF [--from SEQ] [--to SEQ] [--tail N] [OPTIONS]
```

**Options**

* `--jsonl` (default)
* `--out FILE` (write to file instead of stdout)
* `--compress zstd|gzip` (optional)
* `--data-only` / `--meta-only`

This is the “bulk” version of peek/get and is useful for debugging, backups, replay.

---

## `plasmite msg …`

Standalone message file utilities (replacement for bin2json/json2bin)

### `plasmite msg encode`

Convert JSON to binary message form.

```bash
plasmite msg encode --in FILE --out FILE
```

### `plasmite msg decode`

Convert binary message form to JSON.

```bash
plasmite msg decode --in FILE [--out FILE]
```

### `plasmite msg validate`

Validate a binary message file.

```bash
plasmite msg validate FILE
```

Notes:

* If you want “lossless typed rendering” later, add `--lossless` to emit a tagged JSON form. Keep it out of v0.1 unless you discover you need it.

---

# Roadmap (v2+)

## Remote refs + `plasmite serve`

Remote pool refs are explicitly out of scope for v0.1.

When we add them, the CLI UX-level contract is:

* Remote pool refs use `tcp(s)://host:port/POOL` (no trailing slash)
* Subcommands that accept POOLREF should work remotely at least for: `peek`, `poke`, `get`, `export`, and `pool ls`.

`plasmite serve` would expose local pools over TCP with options like:

* `--listen HOST:PORT` (default `0.0.0.0:65456`)
* `--dir PATH`
* `--tls` / `--tls-cert FILE --tls-key FILE`
* `--require-client-cert` (mTLS)
* `--allow-insecure` (explicit)
* `--pidfile FILE`
* `--log FORMAT` (`stderr|pool:NAME|file:PATH`)

## Raw bytes convenience

If we need to store binary payloads, add an explicit, documented wrapper and a `plasmite poke --raw-bytes @FILE` convenience that maps bytes into `data` in a stable way.

---

## `plasmite completion`

Generate shell completions.

```bash
plasmite completion bash|zsh|fish
```

## `plasmite doctor`

Environment and sanity checks.

* prints a JSON object describing pool dir, config, and permission checks
* can validate a pool file quickly
* helpful when debugging “why can’t I see pools”

## `plasmite version`

Print version/build info as JSON.

---

# Golden workflows

### 1) Watch a pool like `peek`

```bash
plasmite peek mypool
```

### 2) Pipe into `jq`

```bash
plasmite peek mypool --jsonl | jq -r '.meta.descrips[]?'
```

### 3) Inject like `poke`

```bash
echo '{"type":"ping","t":123,"source":"cli"}' | plasmite poke mypool --descrip ping
```

### 4) Fetch by seq

```bash
seq=$(plasmite poke mypool '{"x":1}' | jq -r '.seq')
plasmite get mypool "$seq" | jq .
```

### 5) Export last N for debugging

```bash
plasmite export mypool --tail 200 --jsonl > dump.jsonl
```

---

# What we intentionally change from legacy

* **One primary binary** (cleaner docs, completions, consistent flag parsing).
* `p-info` becomes JSON-by-default.
* `p-oldest-idx` / `p-newest-idx` removed; use `pool info` for bounds.
* `p-await` becomes `peek` (no separate concept).
* The trailing-slash remote server quirk is removed (when remote refs land).
* “Pool sleep” becomes an advanced/maintenance concern; only add if actually needed.
