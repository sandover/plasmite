<!--
Purpose: Define the normative v0 contract for Plasmite’s CLI, message schema, and compatibility promises.
Exports: N/A (documentation).
Role: Versioned spec (normative).
Invariants: v0.0.1 “contract” section is frozen once released; future additions require explicit promotion.
-->

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
* `plasmite pool list`
* `plasmite pool delete`
* `plasmite poke`
* `plasmite get`
* `plasmite peek`
* `plasmite version`

### Flags in scope

Minimal, explicit flag set for v0.0.1:

* Global: `--dir`
* `pool create`: `--size`
* `pool list`: no flags
* `poke`: `DATA`, `--file FILE`, `--in`, `--errors`, `--descrip`, `--durability fast|flush`, `--create`, `--create-size`, `--retry`, `--retry-delay`
* `peek`: `--tail`, `--format pretty|jsonl`, `--jsonl`

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
* Streaming: default is pretty JSON per message; `--format jsonl` (or `--jsonl`) emits one object per line.
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

## Notices (non-fatal diagnostics)

This section is **non-normative** until explicitly promoted into the v0.x contract.

Plasmite may emit **notices** on stderr for non-fatal conditions (e.g., drops detected while
streaming). Notices are distinct from errors:

* Notices never change the exit code.
* Notices never write to stdout (stdout remains pure JSON for command outputs).

### TTY stderr (human)

When stderr is a TTY, notices are short, human-readable lines (no JSON required).

### Non-TTY stderr (machine)

When stderr is **not** a TTY, notices are JSON objects with a stable envelope:

```json
{
  "notice": {
    "kind": "drop",
    "time": "2026-02-01T00:00:00Z",
    "cmd": "peek",
    "name": "demo",
    "message": "dropped 3 messages",
    "details": {
      "dropped_count": 3
    }
  }
}
```

Required fields:

* `kind` (string): machine-friendly notice type (e.g., `drop`).
* `time` (string): RFC 3339 timestamp.
* `cmd` (string): CLI subcommand emitting the notice.
* `pool` (string): pool ref or name (path may be included in `details`).
* `message` (string): short human summary (not localized).
* `details` (object): structured fields specific to the notice kind.

Invariants:

* JSON notices never include ANSI escapes, regardless of color policy.
* Colorization of human stderr output and pretty JSON stdout is controlled by `--color auto|always|never`.
* Notice schemas are additive-only once promoted.
* Implementations should coalesce high-frequency notices and rate-limit emissions.

### Notice kinds (current)

* `drop` (from `peek` when messages are overwritten)
  * `details.dropped_count` (number)
* `ingest_skip` (from `poke` when `--errors skip` drops a bad record)
  * `details.mode` (string): `auto|jsonl|json|seq|jq|event`
  * `details.index` (number): 1-based record index
  * `details.error_kind` (string): `Parse`, `Oversize`, or storage error kind
  * `details.line` (number, optional): line number for line-based modes
  * `details.snippet` (string, optional): truncated input excerpt
* `ingest_summary` (from `poke` when `--errors skip` completes)
  * `details.total` (number)
  * `details.ok` (number)
  * `details.failed` (number)

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
* `--since TIME` (RFC 3339 or relative `5m`, `2h`, `1d`)

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

## `plasmite pool list`

List pools.

**Synopsis**

```bash
plasmite pool list
```

**Behavior**

* Output is JSON with a `pools` array.
* Entries are sorted by `name` ascending.
* Non-`.plasmite` files are ignored.
* Unreadable pools are included with an `error` field instead of failing the command.

**Pool entry fields**

* `name` (string): pool name (file stem).
* `path` (string): absolute path to the pool file.
* `file_size` (number): size in bytes (when readable).
* `bounds` (object): `oldest` and/or `newest` seq numbers (when readable).
* `mtime` (string or null): RFC 3339 file modification time (null when unavailable).
* `error` (object, optional): standard error envelope for unreadable pools.

**Example**

```json
{
  "pools": [
    {
      "name": "demo",
      "path": "/Users/me/.plasmite/pools/demo.plasmite",
      "file_size": 1048576,
      "bounds": { "oldest": 1, "newest": 42 },
      "mtime": "2026-02-02T23:40:00Z"
    }
  ]
}
```

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
* `--since TIME`: only emit messages at/after TIME (RFC 3339 or relative `5m`, `2h`, `1d`).
* `--format pretty|jsonl`: select output format (default: `pretty`).
* `--jsonl`: alias for `--format jsonl` (compatibility).
* `--quiet-drops`: suppress non-fatal drop notices on stderr.

**Behavior**

* Without `--tail`, starts at **newest+1** and waits for new messages.
* With `--tail N`, prints the last N messages currently available, then waits for new ones.
* `--since` cannot be combined with `--tail`.
* If `--since` is in the future, `peek` exits with no output.
* Relative `--since` uses UTC now; RFC 3339 offsets are honored.
* If the reader falls behind and messages are overwritten, a non-fatal drop notice is emitted on
  stderr (see “Notices”). Use `--quiet-drops` to suppress.

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
* If stdin is not a TTY and no `DATA`/`--file` is provided: treat stdin as a stream of JSON values (auto-detected format). Each JSON value becomes one message.

**Options (stdin format)**

* `--in auto|jsonl|json|seq|jq` (default: `auto`)
  * `auto`: detect JSON Sequence (0x1e), event-style streams (`data:`/`event:`/`id:`/`:…` lines), or JSON Lines (with multiline recovery).
  * Auto detection precedence (first match wins):
    1) Input prefix contains `0x1e` → `seq`
    2) First non-empty line starts with `data:`/`event:`/`id:`/`:` → `event`
    3) Otherwise → `jsonl` (multiline recovery allowed)
  * Auto detection sniff limits: first 8 KiB and up to 8 lines.
  * `jsonl`: one JSON value per line (best for `jq -c`, log streams, ndjson).
  * `json`: single JSON document (possibly pretty-printed).
  * `seq`: JSON Sequence records separated by 0x1e (common on Linux).
  * `jq`: whitespace-delimited JSON values (jq-style stream).

**Options (stdin errors)**

* `--errors stop|skip` (default: `stop`)
  * `stop`: abort on first parse/append error.
  * `skip`: continue past *parse/oversize* errors at record boundaries; emit notices on stderr; exit code 1 if any record is skipped.
  * Append/storage errors are **not** skippable; they abort ingestion even in `skip` mode.
  * In auto/jsonl multiline mode, `skip` resyncs on lines that look like new JSON values (`{` or `[`); use `--in json` for strict single-document parsing.
  * `skip` is not supported for `--in jq` (no reliable resync).

**Options (durability)**

* `--durability fast|flush` (default: `fast`)
  * `fast`: best-effort (no explicit flush)
  * `flush`: flush frame + header to storage after append

**Options (retries)**

* `--retry N` (default: `0`): retry transient failures up to N times.
* `--retry-delay DURATION` (optional): delay between retries (e.g. `50ms`, `1s`, `2m`).

**Options (create)**

* `--create` : create the pool if it is missing
* `--create-size SIZE` : pool size for creation (bytes or K/M/G)

This gives you the classic pattern:

```bash
seq=$(plasmite poke mypool '{"x":1}' --descrip event | jq -r '.seq')
plasmite get mypool "$seq" | jq .
```

**Examples (stdin)**

```bash
# Single JSON document (pretty JSON ok)
curl -s https://example.com/payload.json | plasmite poke demo --in json

# Event-style stream (data: lines)
curl -N https://example.com/stream | plasmite poke demo

# JSONL from jq
jq -c '.items[]' data.json | plasmite poke demo

# macOS unified log (ndjson)
/usr/bin/log stream --style ndjson --level info | plasmite poke demo --descrip log

# systemd journal JSON
journalctl -o json -f | plasmite poke demo --in jsonl

# systemd journal JSON Sequence (RS-delimited)
journalctl -o json-seq -f | plasmite poke demo --in seq
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
* Subcommands that accept POOLREF should work remotely at least for: `peek`, `poke`, `get`, `export`, and `pool list`.

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
