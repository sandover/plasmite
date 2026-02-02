# Colorized Pretty JSON Output (v0.0.1)

## Goal
Add optional ANSI colorization to *pretty* JSON output so interactive use is easier to scan,
while keeping machine-oriented JSONL output strictly uncolored.

## Constraints
- Must respect `--color auto|always|never`.
- Must not introduce new dependencies.
- Must keep JSONL/streaming output machine-stable (no ANSI).
- Must be readable on light/dark terminals.

## Color scheme
Applies only to **pretty** JSON:

- **Keys** (object property names): Cyan (ANSI 36)
- **String values**: Green (ANSI 32)
- **Numbers**: Yellow (ANSI 33)
- **Booleans**: Magenta (ANSI 35)
- **Null**: Bright black / gray (ANSI 90)
- **Punctuation** (`{ } [ ] : ,`): Dim gray (ANSI 90)

Rationale: matches common conventions (jq/VS Code-like) and avoids red except for errors.

## Implementation approach
Use a small, pure formatter that walks `serde_json::Value` and emits a pretty JSON string
with optional ANSI spans (no regex/post-processing). This avoids new crates and keeps the
colorization stable across platforms.

## Output policy
- `pretty` output: colored when `--color` allows.
- `jsonl` output: never colored.
