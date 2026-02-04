<!--
Purpose: Describe the plasmite doctor diagnostics command.
Exports: N/A (documentation).
Role: User-facing guidance for interpreting validation reports.
Invariants: Exit codes and JSON output shapes match CLI behavior.
Notes: This document is non-normative; the CLI spec is authoritative.
-->

# Diagnostics: plasmite doctor

`plasmite doctor` validates pools and reports corruption or structural issues.

## Usage

```bash
plasmite doctor <POOL>
plasmite doctor --all
```

## Output

- On TTY: human-friendly lines (`OK` or `CORRUPT`).
- When stdout is not a TTY: JSON output with a `reports` array.

## Exit codes

- `0` when all pools are healthy.
- `7` when any pool is corrupt.

## JSON shape (non-normative)

```json
{
  "reports": [
    {
      "pool_ref": "example",
      "path": "/abs/example.plasmite",
      "status": "ok",
      "last_good_seq": null,
      "issue_count": 0,
      "issues": [],
      "remediation_hints": [],
      "snapshot_path": null
    }
  ]
}
```
