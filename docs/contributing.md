# Contributing

## Repo tour (high level)

- `src/`: Rust implementation (CLI + library)
- `spec/`: normative contracts (these are the “source of truth” for public behavior)
- `bindings/`: Go/Python/Node bindings
- `scripts/`: CI and validation helpers (used by `just`)

## Suggested git hooks (optional, recommended)

We provide a small set of local hooks under `docs/suggested-hooks/`:
- `prepare-commit-msg`: appends a commented commit checklist in editor-driven commits
- `pre-commit`: fast local hygiene + quick Rust checks (only when Rust-relevant paths are staged)
- `pre-push`: runs `just ci` to match CI gating before pushing

Install them with:

```bash
./docs/suggested-hooks/install.sh
```

Details: `docs/suggested-hooks/README.md`.

## Releases

Docs of record for release mechanics live at `docs/record/releasing.md`.

Releases are run via a bundled Codex skill:
- Skill location: `skills/plasmite-release-manager/`
- Entry point: `skills/plasmite-release-manager/SKILL.md`

To activate it for Codex locally, symlink the skill into your Codex skills directory:

```bash
CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
mkdir -p "${CODEX_HOME}/skills"
ln -snf "$(pwd)/skills/plasmite-release-manager" "${CODEX_HOME}/skills/plasmite-release-manager"
```

Then ask Codex to use the `plasmite-release-manager` skill for a dry-run release.


