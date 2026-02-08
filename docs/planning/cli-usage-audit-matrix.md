<!--
Purpose: Provide a complete CLI misuse taxonomy and feedback quality rubric.
Exports: Command inventory, misuse classes, and scenario matrix for validation.
Role: Planning artifact for consistent CLI error-message quality improvements.
Invariants: Every current top-level command and pool/serve subcommand is covered.
Invariants: Each command family includes required, invalid, conflicting, unsupported misuse paths.
Notes: Obsolete/pre-release forms are listed explicitly as cruft-removal candidates.
-->

# CLI Usage Audit Matrix

## Command Surface

Top-level commands (current):

- `pool`
- `poke`
- `serve`
- `get`
- `peek`
- `doctor`
- `version`
- `completion`

`pool` subcommands:

- `pool create`
- `pool info`
- `pool delete`
- `pool list`

`serve` subcommands:

- `serve` (run)
- `serve check`
- `serve init`

Excluded obsolete/pre-release forms (do not include in feedback QA scope):

- API-shaped pool refs passed as positional `POOL` values (for example
  `/v0/pools/<name>/append`), which are explicitly rejected.
- Legacy assumption that remote refs accept trailing slash pool URLs.
- Ad hoc debug-only command aliases not present in the Clap command tree.

Cruft candidates to remove or consolidate:

- `peek --jsonl` alias duplicates `peek --format jsonl`; evaluate removing alias
  after compatibility window.
- Repeated remote-ref guidance text across `poke` and `peek`; centralize into
  one reusable validator message template.

## Misuse Taxonomy

- Required argument missing:
  - Required positional omitted (for example `get` without `seq`).
  - Required value omitted after a flag (for example `--token-file` with no path).
- Invalid value:
  - Type/format invalid (bad duration, size, URL, address, enum value).
  - Semantically invalid value (negative or zero where disallowed).
- Conflicting options:
  - Mutually exclusive flags supplied together (for example multiple data sources).
  - Flags that require enabling companion flags (for example dependent option used alone).
- Unsupported combination:
  - Command valid in isolation, invalid for target mode (local vs remote).
  - Feature intentionally unavailable for a subcommand/runtime path.

## Scenario Matrix

| Command | Required path | Invalid path | Conflicting path | Unsupported combination |
| --- | --- | --- | --- | --- |
| `pool create` | `pool create` (no names) | `pool create --size 7MiB demo` (unsupported suffix form) | N/A (no mutual exclusives) | `pool create --index-capacity <too large>` with small `--size` |
| `pool info` | `pool info` (missing name) | `pool info :badref` (invalid pool ref/path form) | N/A | remote URL passed where local name/path expected |
| `pool delete` | `pool delete` (missing names) | `pool delete http://host/pool` (invalid local delete target) | N/A | deleting remote shorthand via local-only delete flow |
| `pool list` | N/A | malformed `--dir` path at top level | N/A | N/A |
| `poke` | `poke` (missing pool) | `poke foo --retry-delay nope` | `poke foo '{"x":1}' --file in.json` | remote pool ref with `--create` |
| `serve` (run) | `serve --token-file` (missing value) | `serve --bind not-an-addr` | TLS flags in invalid pairings for selected mode | non-loopback write access without TLS + token-file unless explicit insecure override |
| `serve check` | inherited required values from provided flags | `serve check --max-body-bytes nope` | conflicting auth/token inputs if both raw token and token-file policy disallows | configuration accepted for check but invalid for runtime bind safety policy |
| `serve init` | `serve init --tls-key` (missing value) | `serve init --bind bad:addr` | N/A | writing artifacts over existing files without `--force` |
| `get` | `get foo` (missing seq) | `get foo not-a-u64` | N/A | remote-style pool refs if command only supports local open path |
| `peek` | `peek` (missing pool) | `peek foo --timeout 3fortnights` | `peek foo --since 5m --tail 10` | remote `peek` with `--since` or `--replay` |
| `doctor` | `doctor` without pool/`--all` | `doctor --all maybe` (spurious value) | `doctor foo --all` | N/A |
| `version` | N/A | `version extra` (unexpected arg) | N/A | N/A |
| `completion` | `completion` (missing shell) | `completion invalid-shell` | N/A | N/A |

Coverage check by argument type:

- Required: all command families with required positionals/flag values covered.
- Invalid: includes format/type violations for addresses, durations, enums, paths.
- Conflicting: includes explicit mutually exclusive and dependency violations.
- Unsupported: includes local-vs-remote and policy-driven invalid combinations.

## Feedback Quality Rubric

A CLI error response passes only if all checks below are true.

- `what_failed`: states exactly what is wrong (argument, value, or combination).
- `where_failed`: points to the offending token/flag/value when available.
- `why_failed`: explains the violated rule (type, conflict, policy, capability).
- `next_step`: gives one concrete corrective action the user can run immediately.
- `example_when_ambiguous`: includes an example command when multiple fixes exist.
- `stable_shape`: for JSON stderr output, preserves stable `error` structure and category.
- `no_blame`: avoids vague or judgmental phrasing; message is operational.

Pass/fail scoring:

- Pass: all rubric checks satisfied.
- Soft fail: missing `example_when_ambiguous` only, but all other checks pass.
- Hard fail: missing `what_failed` or `next_step`, or message hides violated rule.
