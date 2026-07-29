# Vendored dependencies

This directory contains source snapshots of third-party code that we build
directly, rather than fetching at build time.

## Lite3

Lite3 provides the binary JSON representation stored in Plasmite pool frames.
Plasmite builds a curated source snapshot directly so normal builds remain
offline and deterministic.

`lite3.lock.json` records the exact upstream repository, commit, and commit
date. `lite3.sha256` covers every file in the curated snapshot. Run the offline
integrity check with:

```bash
just verify-lite3
```

### Updating Lite3

Review upstream changes and select an immutable, full commit SHA. Refresh the
snapshot with:

```bash
just update-lite3 <40-character-commit>
```

The updater clones upstream into a temporary directory, checks out exactly that
commit, copies a fixed allowlist of build and license files, rewrites the lock
and integrity manifest, and verifies the result. It intentionally excludes
upstream build files, images, examples, and tests that Plasmite does not ship or
compile.

After an update, review the source diff and run `just check`. Add focused
Plasmite regression tests for fixes that affect encoding, decoding, validation,
or memory safety.

Lite3 issue [#20](https://github.com/fastserial/lite3/issues/20) and issue
[#11](https://github.com/fastserial/lite3/issues/11) remain unresolved upstream
risks. The pinned snapshot and Plasmite regression tests reduce exposure at the
integration boundary; they do not resolve or close either issue.

### Tracking upstream

The scheduled CI workflow runs `just check-lite3-upstream`. It fails when
Lite3's `main` branch moves beyond the pinned commit and prints the exact
comparison and update commands. This check accesses the network and is
therefore separate from the deterministic `just check` gate.
