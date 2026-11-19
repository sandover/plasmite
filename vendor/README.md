# Vendored dependencies

This directory contains source snapshots of third-party code that we build
directly, rather than fetching at build time.

## Lite3

Lite3 provides the binary JSON representation stored in Plasmite pool frames.
Plasmite builds the pinned source from this repository. Cargo still needs its
own dependencies in the local cache for an offline build.

`lite3.lock.json` records the upstream repository, commit, commit date and
the path to the local reader-safety patch. `lite3.sha256` covers the curated
snapshot and the patch. Run the offline
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
commit and copies an explicit list of source files, licenses and the upstream
README. It applies `lite3-read-safety.patch` before replacing the snapshot. If
the patch no longer applies, the updater stops before replacing any files.
Otherwise it rewrites the lock and checksums, then verifies the result.

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
