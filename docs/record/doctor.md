# Diagnostics: plasmite doctor

`plasmite doctor` validates pools and reports corruption or structural issues.

## Usage

```bash
plasmite doctor <POOL>
plasmite doctor --all
```

Exactly one scope selector is required:
- pass a pool name/path (`<POOL>`), or
- pass `--all` (but not both).

## Output

- On TTY: human-friendly lines (`OK` or `CORRUPT`).
- When stdout is not a TTY: JSON output with a `reports` array.

## Exit codes

- `0` when all pools are healthy.
- `7` when any pool is corrupt.
- `2` for usage errors (for example, missing scope or conflicting args).

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
