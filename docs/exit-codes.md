<!--
Purpose: Define stable exit codes for Plasmite so scripts can rely on them.
Exports: N/A (documentation).
Role: Contract adjunct referenced by the versioned spec.
Invariants: Exit codes are stable; new kinds must not reuse existing numbers.
-->

# Exit codes

The CLI maps core error kinds to stable exit codes:

| Error kind | Exit code |
| --- | --- |
| Internal | 1 |
| Usage | 2 |
| NotFound | 3 |
| AlreadyExists | 4 |
| Busy | 5 |
| Permission | 6 |
| Corrupt | 7 |
| Io | 8 |
