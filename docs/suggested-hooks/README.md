# Suggested git hooks

This repo includes a small set of **optional** local git hooks under `docs/suggested-hooks/`.

They are designed to:
- keep commits hygienic (no conflict markers, no accidental large/binary blobs),
- nudge docs coherence when changes look user-facing,
- and run the repo’s existing validation commands before push.

CI is still the source of truth; these hooks are just a fast feedback loop.

## What’s included

- `prepare-commit-msg`
  - Appends a commented checklist to editor-driven commit messages (idempotent).
- `pre-commit`
  - Prints a short policy banner.
  - Blocks staged conflict markers.
  - Blocks suspiciously large/binary newly-added files (unless allowlisted).
  - When Rust-relevant paths are staged, runs `cargo fmt --all`, `just clippy`, and `cargo test -q`.
- `pre-push`
  - Runs `just ci` and blocks push on failure (CI-parity gate).

## Install

Recommended: install delegating hooks into `.git/hooks/` that exec the tracked copies in this directory.

```bash
./docs/suggested-hooks/install.sh
```

To overwrite existing hooks:

```bash
./docs/suggested-hooks/install.sh --force
```

## Uninstall

```bash
./docs/suggested-hooks/install.sh --uninstall
```

