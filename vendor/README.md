# Vendored dependencies

This directory contains source snapshots of third-party code that we build
directly, rather than fetching at build time.

## Lite3

- Upstream: https://github.com/fastserial/lite3
- Pinned commit: ac7fc194612fb5d78a978e2c618be4d69fe0fcbb
- Pin date: 2026-01-30

### Update procedure

1. Fetch latest upstream commit.
2. Replace the contents of `vendor/lite3/` with the new snapshot.
   - Remove non-essential assets (examples/tests/img/pc/Makefile) after updating.
3. Update the pinned commit hash above.
4. Run `cargo test` to verify.
