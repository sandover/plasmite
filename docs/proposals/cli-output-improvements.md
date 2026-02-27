# CLI Output Improvements Proposal

Status: proposal
Date: 2026-02-27

## Motivation

The `pool list` output was recently improved with a structured table showing status, bounds, and metadata. This proposal extends that same quality bar across every human-facing CLI output surface. The goal is a CLI that feels coherent, scannable, and genuinely helpful — the kind of tool where the output makes you *want* to keep using it.

The guiding principle: every time plasmite prints something to a terminal, that output should respect the reader's time and attention. No raw byte counts. No data without context. No walls of undifferentiated text.

---

## Current State (audit)

### What's good

- **Error output** is already excellent. The `error:` / `hint:` / `path:` / `caused by:` format with ANSI color is clear, scannable, and actionable. This is the model for the rest of the CLI.
- **`--json` mode** is consistent across commands. Machine-readable output is well-handled.
- **Colorized JSON** on TTY (fetch, follow, version) is a nice touch. The conservative color palette works broadly.
- **Help text** is well-structured with clear mental model, examples, and notes sections.
- **`serve check`** has a clean key-value layout that reads well.

### What needs work

Here is the command-by-command assessment, grouped by severity.

---

## Tier 1: High-impact, straightforward improvements

### 1. `pool create` — silent success feels wrong

**Current output:**
```
NAME   SIZE     INDEX  PATH
demo1  1048576  4096   demo1.plasmite
```

**Problems:**
- No confirmation verb. Was this created? Listed? Found? The table appears without context.
- `1048576` for size — the user typed `--size 1M` (or nothing), they should see `1M` back.
- `4096` for index capacity is meaningless to most users.
- For the common single-pool case, a table is overkill. Tables are for lists.

**Proposed output (single pool):**
```
Created demo1 (1M, 4096 index slots)
  path: demo1.plasmite
```

**Proposed output (multiple pools):**
```
Created 3 pools (1M each, 4096 index slots)
  demo1  demo1.plasmite
  demo2  demo2.plasmite
  demo3  demo3.plasmite
```

**Rationale:** A confirmation verb at the start ("Created") tells the user what happened. Human-readable size (`1M`) mirrors what they typed. The path is secondary context, not the headline.

---

### 2. `pool info` — dense, hard to scan

**Current output:**
```
Pool: demo1
Path: /tmp/plasmite-demo/demo1.plasmite
Size: 1048576 bytes (index: offset=4096 slots=4096 bytes=65536, ring: offset=69632 size=978944)
Bounds: oldest=1 newest=3 count=3
Utilization: used=1536B free=977408B (0.15%)
Oldest: 2026-02-27T17:46:24.075420924Z (9.0s ago)
Newest: 2026-02-27T17:46:24.111079242Z (9.0s ago)
```

**Problems:**
- The `Size:` line packs 6 numbers into one line — offset, slots, bytes, ring offset, ring size, total. This is implementation detail, not operational info.
- Raw byte counts everywhere (`1048576`, `1536B`, `977408B`). These are not scannable.
- `=` delimited key-value pairs inside a `Key: ` prefixed line. Mixed formatting.
- No visual hierarchy. Everything has the same weight.
- Nanosecond-precision timestamps for human reading. Nobody needs 9 decimal places.

**Proposed output:**
```
demo1
  path:         /tmp/plasmite-demo/demo1.plasmite
  size:         1M (1.5K used, 0.2%)
  messages:     3 (seq 1..3)
  oldest:       9s ago   2026-02-27T17:46:24Z
  newest:       9s ago   2026-02-27T17:46:24Z
  index:        4096 slots (64K)
```

**Rationale:**
- Pool name as header, not `Pool: demo1` (the user typed the name, echo confirms).
- Human-readable sizes: `1M`, `1.5K`, `64K`.
- `messages: 3 (seq 1..3)` gives count and range in one glance.
- Relative time first (what you usually care about), absolute time second (when you need to cross-reference).
- Truncate timestamps to seconds for human display (full precision is in `--json`).
- Index stats on their own line, de-emphasized — an advanced detail.
- Consistent 2-space indent, right-aligned labels, left-aligned values.

---

### 3. `pool list` — sizes are raw bytes

**Current output:**
```
NAME   STATUS  SIZE     OLDEST  NEWEST  MTIME                           PATH            DETAIL
demo1  OK      1048576  1       3       2026-02-27T17:46:24.109987535Z  demo1.plasmite
```

**Problems:**
- `1048576` — every pool in the list shows a raw byte count. Should be `1M`.
- `MTIME` shows nanosecond-precision RFC 3339. For a list view, relative time (`3m ago`) or truncated timestamp (`17:46:24`) is more scannable.
- `STATUS` column is always `OK` in the happy path, consuming horizontal space for no signal.
- Empty `DETAIL` column wastes space when there are no errors.

**Proposed output:**
```
NAME   SIZE  MSGS  OLDEST  NEWEST  MODIFIED   PATH
demo1  1M    3     1       3       3m ago     demo1.plasmite
demo2  1M    0     -       -       5m ago     demo2.plasmite
```

**Changes:**
- Human-readable `SIZE` (`1M`, `8M`, `256K`).
- `MSGS` column — message count is the most useful operational metric.
- `MODIFIED` column — relative time for scannability. Full RFC 3339 stays in `--json`.
- Drop `STATUS` and `DETAIL` columns in the default view — surface them only when errors exist. When any pool has errors, show the error rows with an `ERR` marker and detail inline.
- This keeps the happy-path table narrow and scannable.

---

### 4. `pool delete` — table for single delete is heavy

**Current output (single delete):**
```
NAME       STATUS  PATH                DETAIL
throwaway  OK      throwaway.plasmite
```

**Problems:**
- A 4-column table to confirm deleting one pool is a lot of ceremony.
- No confirmation verb.

**Proposed output (single delete):**
```
Deleted throwaway
  path: throwaway.plasmite
```

**Proposed output (multi-delete):**
```
Deleted 2 pools
  foo   foo.plasmite
  bar   bar.plasmite
```

**Proposed output (partial failure):**
```
Deleted 1 of 3 pools
  foo     OK   foo.plasmite
  bar     ERR  not found
  baz     OK   baz.plasmite
```

**Rationale:** Confirmation verbs tell the user what happened. Tables only when multi-item and there's something to compare.

---

### 5. `version` — JSON on TTY is odd

**Current output (TTY):**
```json
{
  "name": "plasmite",
  "version": "0.5.1"
}
```

**Problem:** Every other CLI tool in existence prints `plasmite 0.5.1` (or similar). Printing pretty JSON for `version` on a terminal is surprising. The JSON envelope is valuable for scripts, but on a TTY the user just wants the version.

**Proposed output (TTY):**
```
plasmite 0.5.1
```

**Proposed output (non-TTY / --json):**
```json
{"name":"plasmite","version":"0.5.1"}
```

**Rationale:** Meet expectations. The `--version` / `-V` flag already uses clap's plain text format. The `version` subcommand should match.

---

### 6. `doctor` — minimal output undersells the check

**Current output:**
```
OK: demo1
```

**Current output (--all):**
```
OK: /tmp/plasmite-demo/demo2.plasmite
OK: /tmp/plasmite-demo/demo1.plasmite
```

**Problems:**
- The single-pool case shows only `OK: name`. No stats, no details. The user has no idea what was checked.
- The `--all` case shows full paths (not names) with no alignment.
- No summary. "All 5 pools healthy" would be reassuring.
- Successful validation is the opposite of a neutral event — tell the user their data is sound.

**Proposed output (single pool):**
```
demo1: healthy
  messages:  3 (seq 1..3)
  checked:   3 frames, 0 issues
```

**Proposed output (--all):**
```
All 2 pools healthy
  demo1   3 messages   0 issues
  demo2   0 messages   0 issues
```

**Proposed output (corruption found):**
```
1 of 2 pools has issues

  demo1   healthy      3 messages   0 issues
  demo2   CORRUPT      last_good_seq=41
          truncated frame at offset 8192
```

**Rationale:** Doctor is a trust operation. The user is asking "is my data safe?" — answer that question clearly and give enough context to be confident in the answer.

---

### 7. `feed` receipt — envelope noise

**Current output (single inline feed):**
```json
{"meta":{"tags":[]},"seq":1,"time":"2026-02-27T17:46:24.075420924Z"}
```

**Problem:** On a TTY, this is a JSON blob. For a single feed, the user mostly wants confirmation it worked and the seq number. The `meta.tags: []` is noise when no tags were provided.

**Proposed output (TTY, single feed):**
```
fed seq=1 at 2026-02-27T17:46:24Z
```

Or, for tagged messages:
```
fed seq=3 at 2026-02-27T17:46:24Z  tags: alert
```

**Proposed output (non-TTY or --json):** Unchanged (frozen contract).

**Rationale:** The receipt JSON is a stable contract for scripts. But on a TTY, the human wants a quick confirmation, not a JSON object to parse with their eyes.

---

## Tier 2: Moderate improvements

### 8. `serve init` — good structure, could use polish

**Current output:**
```
serve init: generated artifacts
  token_file: /tmp/plasmite-serve-test/serve-token.txt
  tls_cert:   /tmp/plasmite-serve-test/serve-cert.pem
  tls_key:    /tmp/plasmite-serve-test/serve-key.pem

next commands:
  1. plasmite serve --bind 0.0.0.0:9700 ...
  2. curl -k -sS -X POST ...
  3. curl -k -N -sS ...

notes:
  - token value is not printed; read it from token_file when needed
  - TLS is self-signed; curl examples use -k for local testing
```

**This is close.** Two small improvements:
- The header `serve init: generated artifacts` could be just `Generated TLS + token artifacts` — lead with what was created, not the command name.
- Numbered next-commands could be visually separated with blank lines between them — the long curl commands run together.

---

### 9. `serve check` — already good, minor refinements

**Current output:**
```
plasmite serve check
  status: valid
  listen: 127.0.0.1:9700
  ...
  limits: body=1048576B tail_timeout=30000ms tail_concurrency=64
```

**One issue:** The `limits:` line packs three values into one line with raw bytes again. `body=1M timeout=30s concurrency=64` reads better.

---

### 10. Empty pool list — silence is confusing

**Current output (no pools):**
```
NAME  STATUS  SIZE  OLDEST  NEWEST  MTIME  PATH  DETAIL
```

An empty table with only headers is printed. The user might wonder if something went wrong.

**Proposed output:**
```
No pools found in ~/.plasmite/pools

  Create one: plasmite pool create <name>
```

**Rationale:** An empty state is a teaching moment. Guide the user to the next step.

---

## Tier 3: Systemic patterns

### 11. Human-readable byte formatting (shared utility)

Nearly every command would benefit from a `format_bytes` function:

```
0        -> "0B"
512      -> "512B"
1024     -> "1K"
65536    -> "64K"
1048576  -> "1M"
8388608  -> "8M"
1073741824 -> "1G"
```

Rules: use exact K/M/G when evenly divisible. Otherwise use one decimal: `1.5M`, `256.5K`. Below 1K, show raw bytes. This matches the input format (`--size 1M`) which builds trust — what you put in is what you get back.

---

### 12. Consistent timestamp display

For human output, define two modes:
- **Relative** for list/summary views: `3s ago`, `5m ago`, `2h ago`, `3d ago`
- **Absolute-truncated** for detail views: `2026-02-27T17:46:24Z` (drop sub-second precision)
- **Full precision** stays in `--json` only.

The existing `human_age()` function is a good start. Extend it to cover hours/days/weeks.

---

### 13. Confirmation verbs for mutating commands

Every command that changes state should start its human output with a past-tense verb:

| Command | Verb |
|---------|------|
| `pool create` | `Created` |
| `pool delete` | `Deleted` |
| `feed` (single) | `Fed` / `Sent` |
| `feed` (stream) | `Fed N messages` |
| `serve init` | `Generated` |

This is a small thing that makes a big difference. It answers "did it work?" before the user even has to ask.

---

## Design principles (from this audit)

1. **Verbs first.** Mutating commands confirm what happened: "Created", "Deleted", "Fed".
2. **Human units.** Bytes become `K`/`M`/`G`. Timestamps lose nanoseconds. Ages read as relative.
3. **Tables for lists, prose for singles.** Creating one pool? One-liner. Creating five? Table.
4. **Empty states teach.** No pools? Say so, then show the next command.
5. **TTY vs pipe.** Human output on TTY, stable JSON on pipe. Same data, different rendering.
6. **Detail on demand.** `pool info` shows operational essentials by default. `--json` gives everything.
7. **Match input to output.** User types `--size 1M`, sees `1M` back. Builds trust.

---

## Implementation plan

### Phase 1: Shared utilities
- [ ] `format_bytes(u64) -> String` — human-readable byte formatting
- [ ] `format_timestamp_human(rfc3339: &str) -> String` — truncate to seconds
- [ ] `format_relative_time(age_ms: u64) -> String` — extend `human_age` to cover hours/days
- [ ] `format_seq_range(oldest: Option<u64>, newest: Option<u64>) -> String`

### Phase 2: Pool commands
- [ ] `pool create` — verb + inline summary (single), or verb + table (multi)
- [ ] `pool info` — restructured key-value with human units
- [ ] `pool list` — human-readable sizes, relative mtime, message count column
- [ ] `pool delete` — verb + inline summary, table only for multi with mixed results
- [ ] Empty-state handling for `pool list`

### Phase 3: Message commands
- [ ] `feed` receipt — human-readable one-liner on TTY
- [ ] `version` — plain text on TTY

### Phase 4: Diagnostic commands
- [ ] `doctor` — richer per-pool detail, summary line for `--all`
- [ ] `serve init` — minor polish (header, spacing)
- [ ] `serve check` — human-readable limits

### Constraints
- **No JSON contract changes.** All `--json` output stays identical.
- **No flag changes.** Existing CLI surface is frozen.
- **TTY-only changes.** All improvements are gated on `io::stdout().is_terminal()`.
- **Additive.** Old scripts piping non-TTY output see no change.

---

## Non-goals

- Color in table output (not needed; alignment and hierarchy do the work)
- Spinners, progress bars, or animation (plasmite operations are fast)
- Emoji (not the aesthetic; let the data speak)
- Interactive prompts (plasmite is a batch/pipe tool)
