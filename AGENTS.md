This repository inherits global agent policy from `/Users/brandonharvey/AGENTS.md`.
Keep this file limited to plasmite-specific deltas.

# CI hygiene (required before pushing code)
- Run `cargo fmt --all`.
- Run `cargo clippy --all-targets -- -D warnings`.
- Do not add new `#[allow(clippy::...)]` without explicit justification in the commit body.
