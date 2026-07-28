# CLI and Help System Audit

Status: audit complete; parsimonious reform plan revised

Audited: 2026-07-27

Surface: Plasmite 0.6.2 CLI, documentation, errors, and tests

## Executive summary

Plasmite's CLI behavior is substantially better tested than its help system.
The implementation has deep integration coverage, actionable runtime errors,
and a useful root-level first workflow. The help system nevertheless has three
structural problems:

1. No source owns the complete cross-command operating model.
2. Generated command help has expanded into the missing manual while still
   omitting material runtime constraints.
3. Help correctness is protected by spot checks rather than focused,
   mechanical coverage of the public command surface.

The recommended reform follows the principles proven in Ergo:

- Root help is the front door: mental model, first workflow, exact command
  inventory, top-level options, and navigation.
- One guide owns the complete cross-command model.
- Generated command help owns syntax, options, command-specific input and
  output, and material constraints. It is not a third manual.
- The cookbook owns recipes, not the command contract.
- The specification owns compatibility guarantees, not onboarding prose.
- Focused tests protect the visible command inventory, important constraints,
  and documentation links without turning prose into a second contract.

This is primarily an information-architecture and discoverability reform. A
few findings also require CLI design decisions because current help contradicts
runtime behavior or because nominally global behavior is not actually global.

## Audit method

The audit compared:

- the complete Clap command declarations in `src/main.rs`;
- runtime validation and dispatch in `src/command_dispatch.rs`, `src/serve.rs`,
  and `src/serve_init.rs`;
- live `-h` and `--help` output for every public command path;
- TTY and non-TTY behavior for representative output and error paths;
- the root README, docs index, cookbook, vision, serving guide, CLI spec, and
  current proposal material;
- CLI integration tests and the CLI UX and cookbook smoke scripts.

The review enumerated commands, arguments, flags, input channels, output modes,
target restrictions, requires/conflicts rules, adaptive TTY behavior, special
exit statuses, documentation ownership, and test coverage. It did not change
the CLI.

## Principles for the reformed system

### Every surface has one job

The manuals should be complete together without repeating one another:

- **Root help:** orientation and navigation.
- **CLI guide:** complete operating model and cross-command behavior.
- **Command help:** exact local contract for invoking one command.

README, cookbook, serving guidance, specification, and errors consume this
contract for their own purposes. They should not become competing manuals.

### Discover behavior before failure

An actionable error is necessary but insufficient. If a command rejects a flag
combination, requires another option, changes behavior by input channel, or
supports only local or remote targets, that fact belongs in help before the
user discovers it at runtime.

### Test meaning, not prose snapshots

Tests should protect command inventory, input and output channels, option
constraints, capability boundaries, and reader navigation. They should not
freeze line wrapping or incidental wording.

### Preserve progressive capability

The first local workflow should remain simple. Remote authentication, replay,
MCP, process capture, and serving should be discoverable without overwhelming
the root help or the local create/feed/follow path.

## Current surface inventory

### Public command paths

The current tree exposes 18 public command/help paths below the root:

| Family | Paths |
| --- | --- |
| Pool management | `pool`, `pool create`, `pool info`, `pool delete`, `pool list` |
| Messaging | `feed`, `fetch`, `follow`, `duplex` |
| Process capture | `tap` |
| Serving | `serve`, `serve init`, `serve check` |
| Agent integration | `mcp` |
| Maintenance | `doctor` |
| CLI support | `version`, `completion`, built-in `help` |

The root inventory is accurate at the family level. The README command
overview is not: it omits `tap`, `mcp`, `version`, and the `serve init` and
`serve check` paths, while mentioning completion only after the table.

Cargo also discovers `plasmite-conformance` as a third executable beside
`plasmite` and `pls`. It is not part of the primary command tree or release SDK
layout and is assessed separately below.

### Arguments and flags

| Command | Arguments | Command options |
| --- | --- | --- |
| root | command path | `--dir`, `--color`, `--help`, `--version` |
| `pool create` | one or more names | `--size`, `--index-capacity`, `--json` |
| `pool info` | name or path | `--json` |
| `pool delete` | one or more names or paths | `--json` |
| `pool list` | none | `--json` |
| `feed` | pool ref, optional inline JSON | `--tag`, `--file`, `--durability`, `--create`, `--create-size`, `--retry`, `--retry-delay`, `--in`, `--errors`, remote auth/TLS options |
| `fetch` | local pool name/path, sequence | none |
| `follow` | pool ref | `--create`, `--tail`, `--one`, `--jsonl`, `--timeout`, `--data-only`, `--format`, `--since`, `--where`, `--tag`, `--quiet-drops`, `--no-notify`, `--replay`, remote auth/TLS options |
| `tap` | local pool ref, wrapped command | `--create`, `--create-size`, `--tag`, `--quiet`, `--durability` |
| `duplex` | pool ref | `--me`, `--create`, `--tail`, `--jsonl`, `--timeout`, `--format`, `--since`, `--echo-self` |
| `doctor` | optional pool | `--all`, `--json` |
| `serve` | optional `init` or `check` subcommand | connection, authentication, TLS, CORS, and safety options |
| `serve init` | none | `--bind`, output paths, `--force` |
| `serve check` | none | `--json`; effective serve options currently belong to the parent |
| `mcp` | none | command-scoped `--dir` |
| `version` | none | none |
| `completion` | shell | none |

### Input channels

Plasmite has several materially different input contracts:

- `feed` accepts inline `DATA`, a file, `--file -`, or implicit piped stdin.
- `feed --in` and `--errors` govern streamed file input as well as stdin.
- `tap` receives an argv vector after a mandatory `--` separator and inherits
  the wrapped process's stdin.
- `duplex` treats TTY stdin as line-oriented chat and non-TTY stdin as a JSON
  stream.
- `mcp` consumes newline-delimited JSON-RPC on stdin until EOF.
- `serve init` writes token and TLS artifacts to the filesystem.
- Pool names, explicit paths, and remote shorthand URLs are not supported
  uniformly across commands.

These facts do not currently have one authoritative home.

### Output and exit channels

The CLI produces:

- human-readable tables and summaries;
- JSON selected with `--json`;
- streaming JSON Lines selected with `--format jsonl` or `--jsonl`;
- adaptive TTY/non-TTY output for some commands;
- plain or JSON diagnostics depending on stderr being a TTY;
- completion scripts;
- long-running HTTP and MCP transports.

In addition to the general error-kind exit mapping, special statuses include:

- `follow` and `duplex` timeout: 124;
- partial `feed --errors skip` failure: 1;
- `tap`: wrapped process status or signal-derived status;
- `pool delete`: nonzero if any requested deletion fails;
- `doctor`: corruption status when any inspected pool is corrupt.

The CLI spec points at source code for the general exit mapping, and no
installed manual owns the complete output/exit model.

## What is working well

- Root help gives a new user a complete local create/follow/feed round trip.
- The mental model uses the central `feed`, `follow`, and `fetch` verbs.
- Most runtime errors are concise and actionable.
- Long help frequently documents local/remote differences and safety posture.
- `tap` explicitly describes its local-only scope and mandatory `--`
  separator in prose.
- Integration tests deeply exercise TTY and non-TTY output, streaming input,
  partial failures, remote restrictions, replay, serve security, and child
  process status propagation.
- `scripts/cli_ux_tour.sh` provides a useful human-output review harness.
- `scripts/cookbook_smoke.sh` validates representative recipes.

These are strong foundations. The reform should organize and protect them,
not replace them with a large new documentation framework.

## Findings

Severity means:

- **P0:** current help directly contradicts behavior, or an accepted option is
  silently ignored.
- **P1:** a material behavior or navigation path is undiscoverable or
  structurally ambiguous.
- **P2:** drift, duplication, or clarity debt likely to cause future defects.

### P0: `version` promises JSON but adapts to the terminal

`version` is described as “Print version info as JSON” and its long help says
it emits stable machine-readable JSON. At runtime it prints plain
`plasmite X.Y.Z` when stdout is a TTY and JSON only when stdout is non-TTY.

This is a direct output-contract contradiction. The separate root
`--version` path also emits plain text, producing three related interfaces
whose distinctions are not explained by root help.

Retain the established adaptive behavior and state the exact TTY contract in
command help and the CLI guide. Changing runtime output or adding another
format option is unnecessary for this help correction and could affect
existing scripts.

### P0: `tap` renders its required command as optional

Live usage renders:

```text
plasmite tap [OPTIONS] <POOL> [-- <COMMAND>...]
```

Runtime behavior requires a wrapped command after `--` and returns a usage
error without one. The prose and error are correct, but the formal syntax says
the command is optional.

The usage signature should make the command required, and a focused help test
should connect that signature to the runtime requirement.

### P1: no document owns the complete CLI operating model

The docs index identifies docs of record and specifications but no CLI manual.
The docs-of-record index likewise has no general CLI guide. The CLI spec calls
`docs/record/vision.md` the “CLI docs of record,” but the vision describes
product principles rather than command operation.

The result is an ownership vacuum:

- root help teaches only the first workflow;
- generated help carries extensive operational prose;
- the cookbook teaches selected use cases;
- serving guidance owns deployment-specific behavior;
- the spec freezes only selected script guarantees.

None answers, in one maintained place:

- which commands accept local paths, names, or remote refs;
- how input source selection works;
- how TTY and non-TTY output differs;
- which output is stable for scripts;
- how filtering, replay, timeout, and drop behavior compose;
- which flags require or conflict with one another;
- what exit statuses scripts should expect.

### P1: generated help is an uneven third manual

The most complex commands embed long descriptions, examples, and notes in
Clap declarations:

| Command | `-h` lines | `--help` lines |
| --- | ---: | ---: |
| `feed` | 30 | 103 |
| `follow` | 34 | 108 |
| `serve` | 42 | 89 |

The short and long help variants therefore expose different levels of
knowledge. Long help duplicates cookbook and serving material, but neither
variant is complete enough to replace a cross-command guide.

Consequences:

- operating guidance is coupled to Rust command declarations;
- examples are repeated across root help, command help, README, and cookbook;
- users must know that `--help` contains facts absent from `-h`;
- command-local prose carries cross-command concepts such as remote-ref grammar
  and output selection;
- adding a flag can silently create a new omission without failing tests.

### P0: `serve init` silently ignores parent configuration

`serve init` defines its own `--bind`, but parent serve flags placed before
`init` are parsed into `ServeRunArgs` and then discarded by the init dispatch.
A command such as:

```text
plasmite serve --bind 10.0.0.1:9700 init
```

accepts a bind value that does not configure init's printed commands. Reject
parent serve options before `init` when they would otherwise be ignored. This
is a narrow correctness fix: an accepted option must not silently do nothing.
Implement the guard in dispatch by comparing `ServeRunArgs` with its effective
defaults and returning a usage error that names the first non-default ignored
option. This needs no new parser abstraction. `serve` is explicitly non-frozen
in `spec/v0/SPEC.md`, so tightening an invocation that currently lies about its
effect is compatible with the documented v0 boundary.

### P1: `serve check` hides the configuration it checks

`plasmite serve check --help` displays only `--json`. The connection,
authentication, TLS, CORS, and safety flags being validated belong to the
parent `serve` command and must appear before `check`:

```text
plasmite serve --bind 127.0.0.1:9701 check --json
```

The locally rendered usage and option list do not reveal this. Examples hint
at the placement, but help should not require reverse-engineering an example
to discover the command's primary inputs.

Keep the existing option placement and state it directly in `serve check`
help. Accepting a duplicate set of options after `check` would introduce
precedence and parser complexity without evidence that it is needed.

### P1: top-level options are called global but are positional

The spec calls `--dir` global, but Clap does not mark `--dir` or `--color` as
global. They must precede the command:

```text
plasmite --dir /tmp/pools pool list   # accepted
plasmite pool list --dir /tmp/pools   # rejected
```

They also disappear from subcommand help. `mcp` independently defines another
`--dir`, so both of these are accepted, with the command-scoped value taking
precedence:

```text
plasmite --dir A mcp
plasmite mcp --dir B
```

Either make these flags genuinely global, or consistently call them
top-level options and document placement and precedence.

### P1: material requires/conflicts rules are missing

The following table compares runtime behavior with generated help:

| Command | Implemented rule | Current discoverability |
| --- | --- | --- |
| `pool create` | index region may consume at most 50% of pool size | Runtime error only |
| `feed` | `--create-size` requires `--create` | Runtime error only |
| `feed` | `--retry-delay` requires nonzero `--retry` | Runtime error only |
| `feed` | exactly one of inline `DATA`, `--file`, or stdin | Partial: data/file conflict is parser-backed; full input selection is prose/runtime |
| `feed`, `follow` | `--token` conflicts with `--token-file` | Runtime error only |
| `follow` | `--jsonl` conflicts with any `--format` | Runtime error only |
| `follow` | `--since` conflicts with `--tail` | Parser-enforced but not rendered |
| `follow` | replay requires `--tail` or `--since`; speed must be finite and nonnegative | History requirement shown; numeric constraints omitted |
| `tap` | `--create-size` requires `--create` | Runtime error only |
| `duplex` | `--jsonl` conflicts with any `--format` | Runtime error only |
| `duplex` | `--since` conflicts with `--tail` | Parser-enforced but not rendered |
| `duplex` | TTY mode requires `--me` | Long prose only; formal syntax says optional |
| `doctor` | exactly one of `POOL` and `--all` is required | Usage renders both optional |
| `serve` | `--token` conflicts with `--token-file` | Runtime error only |
| `serve` | `--tls-cert` and `--tls-key` require one another | Runtime error only |
| `serve` | `--tls-self-signed` conflicts with cert/key paths | Runtime error only |
| `serve` | body, timeout, and concurrency limits must be positive | Runtime error only |
| `serve init` | token, certificate, and key paths must be distinct | Runtime error only |

Where Clap can express a relationship, parsing should enforce it in the
declaration. Clap does not automatically explain every relationship in
generated help, so material constraints still need concise help text.
Target-dependent or compound rules need runtime validation and focused tests
for the most consequential cases.

### P1: local and remote capability is fragmented

The current capability model is:

| Command | Local name/path | Remote shorthand | Remote auth/TLS |
| --- | --- | --- | --- |
| Pool CRUD | yes | no | no |
| `feed` | yes | yes | token, token file, CA, skip verify |
| `fetch` | yes | no | no |
| `follow` | yes | yes | token, token file, CA, skip verify |
| `tap` | yes | no | no |
| `duplex` | yes | yes | none |
| `doctor` | yes | no | no |

For `feed` and `follow`, remote auth/TLS options are rejected when used with a
local ref. Their help heading implies remote scope but does not state the
rejection or token-option conflict.

Remote `follow` rejects `--create`, `--since`, `--replay`, `--no-notify`, and
`--quiet-drops`. Long help lists a supported subset and explicitly explains
some exclusions, but the complete capability boundary is not stated in one
place. The CLI spec lists `--since` and `--replay` but omits the notify/drop
restrictions.

`duplex` advertises remote operation but exposes no token, token-file, CA, or
skip-verification options. It therefore cannot connect to the authenticated or
custom-CA remote server produced by Plasmite's recommended secure serving
workflow. This is a capability gap, not merely a documentation omission.

The complete matrix belongs in the CLI guide; each command's relevant row
belongs in command help.

### P1: input, output, and exit contracts are incomplete

#### `feed`

- `--in` is described as controlling stdin streams, but it also controls
  streamed file input.
- `--errors` likewise applies to both sources.
- `--errors skip` continues ingestion but exits 1 if any record failed; help
  does not disclose the status.
- Feed receipts adapt between human and JSON according to stdout TTY state,
  without an explicit output flag or local help contract.

#### `tap`

`tap` forwards the wrapped process's exit status (or a signal-derived status)
and records start, line, and exit messages. Help explains capture but not the
exit propagation or record schema. The architecture describes the adapter, but
operators need the message and exit contract.

#### `serve init`

Output is human-readable on a TTY and JSON otherwise, with no `--json` override.
Help does not explain this adaptive contract.

#### Errors

The spec says non-TTY errors are JSON, but missing required arguments or
subcommands are special-cased as plain Clap help on stderr with exit 2. Either
document the help-on-missing exception or normalize the behavior.

#### Installed navigation

`serve --help` directs users to the repository-relative path
`spec/remote/v0/SPEC.md`. That path is not useful in most installed contexts.
Use a durable URL for the canonical guide.

### P2: the two binary names are not behaviorally identical

The README says `pls` and `plasmite` are the same binary. `pls` is actually a
wrapper that launches `plasmite`. With no arguments:

| Invocation | Exit | Help channel |
| --- | ---: | --- |
| `plasmite` | 2 | stderr |
| `pls` | 0 | stdout |

The aliases should have one documented no-argument contract, or the README
should describe `pls` as a convenience wrapper with the intentional
difference.

### P2: `plasmite-conformance` has no declared public status

Cargo metadata exposes three binary targets:

- `plasmite`;
- `pls`;
- `plasmite-conformance`.

Release SDK packaging intentionally copies only `plasmite` and `pls`, but
ordinary Cargo installation can expose the auto-discovered conformance runner.
That runner manually parses one manifest path and treats `--help` as a
filename, producing a file-read error rather than help.

Decide whether it is:

- a developer-only test runner, in which case package metadata should prevent
  accidental public installation; or
- a supported executable, in which case it needs normal help, version,
  documentation, and distribution treatment.

Defer that packaging decision from the help reform; it does not block ordinary
`plasmite` command discovery.

### P2: documentation has concrete drift

- The README command overview omits public command paths noted above.
- README and cookbook links point to “pattern matching” and “follow” sections
  in the CLI spec that do not exist.
- The docs index omitted the cookbook despite its top-level reference role
  (fixed by this audit patch).
- The CLI spec points to the vision as the CLI docs of record.
- The spec's list of implemented non-frozen commands omits MCP and completion
  and does not represent the serve subcommands.
- The spec says non-streaming commands provide `--json`. `fetch` instead emits
  JSON unconditionally, while `version` emits JSON only when stdout is not a
  terminal; the three behaviors should be described separately.
- The spec still lists only macOS and Linux while the distribution record
  describes official Windows CLI delivery.

These are symptoms of unclear ownership rather than isolated copy errors.
Fixing them individually without first assigning roles will invite recurrence.

### P2: help tests are spot checks, not a system contract

The integration suite has broad behavior coverage, but help-specific tests
currently verify:

- one root command row and one root example;
- built-in help availability;
- one pool subcommand row;
- no-argument help for a subset of commands;
- detailed help content for `tap`;
- existence of `serve init` help.

There is no test for:

- exact recursive command inventory;
- successful help for every public command path;
- complete visible flag inventory;
- discoverability of every material runtime constraint;
- command input/output channels;
- local/remote capability coverage;
- root/guide/command-help role separation;
- agreement between docs and the live command tree;
- valid documentation links and anchors.

The existing behavior tests and UX scripts should remain. They solve a
different problem.

## Recommended information architecture

### 1. Root help: the front door

Keep root help short and require it to contain:

1. Product scope in one sentence.
2. Pool/message mental model.
3. One complete local workflow: create, follow, feed, inspect.
4. Exact command families and every top-level command.
5. Top-level options, including their placement until they become global.
6. A one-paragraph human-versus-machine output orientation.
7. Navigation to command help, the complete guide, and recipes.

Remote access should be visible as an opt-in capability, not introduced into
the first local workflow.

### 2. CLI guide: the complete operating model

Add a canonical `docs/cli.md`. Link it from root help using a durable URL and
from the repository documentation index. Do not add a CLI command for the
guide: that would create another surface, packaging concern, and test
obligation without evidence that a command is needed.

The guide should stay narrow and own only cross-command behavior that has no
clear home today:

- pool-ref resolution and the local/remote capability matrix;
- input selection across inline JSON, files, stdin, TTY duplex, and tap argv;
- human, JSON, JSONL, TTY-adaptive, and streaming output;
- errors, top-level option placement, and special exit statuses.

It should link to the cookbook for workflows and filtering recipes, the serving
guide for deployment and MCP, and the compatibility specs for stable
guarantees. It should not repeat the pool model, tutorials, serving progression,
MCP operation, or every command flag.

### 3. Generated command help: the local invocation contract

Every command help page should contain only:

- exact usage;
- arguments and options;
- input source and stdin/TTY behavior where applicable;
- output form and special exit behavior;
- required, repeatable, defaulted, conflicting, and conditional options;
- local/remote scope for that command;
- navigation to the guide for cross-command concepts.

Use compact `INPUT`, `OUTPUT`, and `CONSTRAINTS` sections where the option table
cannot express the behavior. Remove tutorial sequences and large example sets
from generated help after the guide owns them.

`-h` and `--help` should ideally differ only in formatting detail, not in
essential facts. No material constraint should be available only through the
long variant.

### 4. Supporting documents

| Surface | Role after reform |
| --- | --- |
| README | Product landing page, smallest happy path, installation, navigation |
| Cookbook | Copy/paste recipes and use-case composition |
| Serving guide | Deployment, TLS/auth, proxies, CORS, browser and remote operations |
| CLI spec | Stable script contract, compatibility boundaries, machine formats |
| Vision | Product goals and invariants |
| Architecture | Maintainer implementation model |
| Errors | Contextual recovery after a failed invocation |

## Recommended CLI corrections

Separate these into compatibility-safe help work and behavior decisions.

### Immediate correctness fixes

- Make `version` help state its existing TTY-adaptive behavior; do not change
  the established runtime output contract during help cleanup.
- Make `tap` usage render the wrapped command as required.
- Reject parent serve options before `init` when init would ignore them.
- Repair broken documentation links and current-state drift.

### Additive or prose-only

- Add the canonical CLI guide and durable navigation to it.
- Replace large generated tutorials with exact input/output/constraint text.
- State all current runtime relationships in help.
- Add relevant Clap `requires`, `conflicts_with`, and argument groups where
  they preserve existing accepted/rejected invocations; add concise help text
  separately because Clap does not render every relationship.
- State in `serve check` help that serve configuration options precede
  `check`; retain the existing parser placement.
- Consistently call `--dir` and `--color` top-level options and document their
  placement. Do not make them global while `mcp` owns a command-scoped `--dir`.
- Replace repository-relative installed-help links with durable navigation.
- Repair docs indexes, command inventory, spec references, and broken anchors.

### Explicit compatibility decisions

- Whether no-argument `pls` and `plasmite` behavior should converge.
- Whether `plasmite-conformance` is private tooling or a supported executable.
- Whether `duplex` gains the remote auth/TLS options needed for the recommended
  secure server.
- Whether help-on-missing remains plaintext on non-TTY stderr.
- Whether all non-streaming commands should share an explicit `--json`
  convention.

Do not hide these choices inside a documentation cleanup. They change
observable behavior and should be checked against the v0 policy.

## Focused validation plan

Keep validation mechanical and small; runtime semantics remain owned by
behavioral tests.

### Exact command inventory

In a `src/main.rs` unit test, where the private `Cli` type is available,
recursively derive the visible Clap command tree and assert that:

- every intended public path exists;
- removed or accidental commands do not exist;
- root help lists every top-level command exactly once.

### Every command has usable help

For every group and leaf:

- `--help` exits zero;
- usage contains the exact command path;
- every visible argument and flag is rendered;
- required-command no-argument behavior shows that command's help.

### Material constraints are discoverable

Use a short table of stable fragments only for material rules that are not
already visible through generated syntax or covered by behavioral tests:

- doctor pool/`--all` exclusivity;
- create-size/create and retry-delay/retry;
- replay history and local-only limits;
- positive server limits and distinct init artifact paths;
- index capacity limits;
- tap's required separator and command.

### Documentation integrity

Validate:

- relative documentation links resolve.

Keep example execution in `scripts/cookbook_smoke.sh`; do not build another
syntax checker or Markdown validation framework.

## Suggested implementation sequence

1. **Fix direct contradictions.** Correct version and tap help, reject ignored
   serve-init parent options, and repair broken links.
2. **Freeze the mechanical inventory.** Add command-tree introspection in a
   `src/main.rs` unit test and retain process-level help smoke in the CLI
   integration suite.
3. **Create the guide.** Move genuinely cross-command knowledge into
   `docs/cli.md`, then link it from root help and the docs index.
4. **Tighten command help.** Retain syntax, input/output behavior, material
   constraints, and local/remote scope; remove only clearly duplicated
   tutorials.
5. **Validate.** Run documentation integrity checks, the CLI UX tour, `just
   check`, and the relevant integration gates.

## Definition of complete

The reform is complete when:

- a new user can perform a local create/feed/follow round trip from root help;
- root help lists and routes every top-level capability;
- one narrow guide explains the four orphaned cross-command topics;
- every command's local help reveals all of its arguments, flags, input/output
  channels, target restrictions, and material constraints;
- no essential fact appears only in an error or a long-help variant;
- README, cookbook, serving guide, spec, and installed help agree on current
  syntax and ownership;
- focused tests fail when the visible command/flag inventory, important
  constraints, or documentation links drift;
- behavioral integration tests continue to protect the runtime contract.
