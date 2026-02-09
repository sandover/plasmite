# CLI usage feedback audit

## Scope

- In-scope command families: `pool`, `poke`, `peek`, `get`, `doctor`, `serve`,
  `completion`, `version`.
- Error mode covered: non-interactive stderr JSON output (current CI-enforced
  path in `cli_integration`).
- Out of scope: obsolete pre-release invocation forms and non-current aliases not
  present in the Clap command tree.

## Methodology

1. Executed misuse scenarios defined in `docs/planning/cli-usage-audit-matrix.md`.
2. Verified baseline behavior with `cargo test --test cli_integration -- --nocapture`.
3. Re-ran representative commands directly against `target/debug/plasmite` to
   capture observed error payloads.
4. Scored each gap by severity (P0/P1/P2) and confidence.

Severity rubric:

- P0: user-dangerous or data-loss-prone guidance gap.
- P1: likely to block common workflows without clear remediation.
- P2: quality/consistency issue with recoverable path.

## Coverage

| Command family | Scenario type sampled | Status |
| --- | --- | --- |
| `pool` | missing required positional | Covered |
| `poke` | unsupported dependent flag combination | Covered |
| `peek` | missing required positional | Covered |
| `get` | missing required positional | Covered |
| `doctor` | conflicting options | Covered |
| `serve` | invalid bind value | Covered |
| `completion` | invalid enum value | Covered |
| `version` | unexpected extra argument | Covered |

## Findings

### F1: Missing-required diagnostics omit offending argument names (P2)

- Reproduction: `target/debug/plasmite pool info`
- Observed output:
  - `message`: `the following required arguments were not provided:`
  - `hint`: `Try \`plasmite pool info --help\`.`
- Expected helpful output:
  - Message should name missing token (for example `missing required argument: <NAME>`).
  - Hint should remain, but message should be immediately actionable without opening help.
- Severity: P2
- Confidence: 0.93

### F2: Same missing-required issue appears in `get` path (P2)

- Reproduction: `target/debug/plasmite get demo`
- Observed output:
  - `message`: `the following required arguments were not provided:`
  - `hint`: `Try \`plasmite get --help\`.`
- Expected helpful output:
  - Message should include missing positional (`<SEQ>`), not only a generic clause.
  - Keep concise single-line wording for consistency with current JSON/TTY style.
- Severity: P2
- Confidence: 0.91

### F3: `completion` invalid-shell hint points to top-level help (P2)

- Reproduction: `target/debug/plasmite completion nope`
- Observed output:
  - `message`: `invalid value 'nope' for '<SHELL>'`
  - `hint`: `Try \`plasmite --help\`.`
- Expected helpful output:
  - Hint should prefer subcommand-local remediation (for example
    `Try \`plasmite completion --help\``).
- Severity: P2
- Confidence: 0.88

## Recommended Fix Queue

First-wave cutoff policy: include P0 + P1 only.

- First-wave (P0/P1): none identified in current audit run.
- Next-wave (P2 quality consistency):
  1. Update clap error summarization for missing required args to include argument names.
  2. Normalize help hints to command-local forms (`<command> --help`) when subcommand context exists.
  3. Add targeted tests for missing-required argument name presence in `pool info` and `get`.

## Residual Risks

- TTY-specific stderr rendering is not directly exercised in this audit; JSON path is
  strongly covered, but terminal rendering parity could still drift.
- Multiple-invalid-input precedence (which error is reported first) is only partially
  sampled and may vary by Clap parser changes.
- Remote-only misuse permutations are represented by key cases, not full cross-product
  coverage for all flag combinations.
