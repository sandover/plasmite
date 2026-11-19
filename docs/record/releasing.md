# Releasing Plasmite

This document sets release and compatibility policy and explains why. The [release runbook](../../skills/plasmite-release-manager/SKILL.md) owns the commands, their order and recovery steps.

## Release contract

A release ships one reviewed commit through GitHub SDK archives, crates.io, npm, PyPI, Homebrew and the Go module. They share one version. `release/targets.json` defines the native target matrix. [Distribution](distribution.md) describes the channels and prerequisites.

- Honor the requested review boundary and any publication hold.
- Before tagging, require `just release-gate`, a current `just audit`, and a passing `ci.yml` run for the exact release commit.
- For hot-path or storage changes, compare benchmarks against the previous release on the same host. Routine patches need no benchmark.
- Commit and push the complete candidate before tagging. Never move a release tag.
- Track failed gates in Ergo under `Release blockers: <release_target>`. A failed or incomplete required gate blocks the release.
- Record the target, source commit, build and publish run IDs and evidence, so recovery resumes the intended release instead of whichever workflow ran last.

## Release notes

Add release-worthy outcomes to `CHANGELOG.md` under Unreleased as work lands, then move them into a versioned section when preparing the release; the publish workflow uses that section. Say what users can rely on, which failure or cost changed, and any action an upgrade requires. Group implementation work around those outcomes. Symbol-by-symbol migration belongs in the upgrade guide and investigation detail in Ergo. Scope each performance claim to what its measurements support.

## Build and publication

Building and publishing use separate workflows. A credential failure can resume publication from the verified build artifacts while they remain available.

`release.yml` builds the tagged commit with a pinned Rust toolchain, smoke-tests every matrix target and stores the SDKs, Python distributions, npm package and source metadata as artifacts. A successful build publishes nothing.

`release-publish.yml` takes a successful build that matches the release tag and source commit. It checks the artifacts and registry preflight, renders a temporary Homebrew candidate, publishes to the registries in parallel, and creates the GitHub Release only after all of them succeed. The crates.io job packages the tagged Rust source, and npm and PyPI publish the selected build artifacts. Anonymous public downloads must match the build's checksums. The workflow reads the manifest and changelog from the release's own source, so retrying an older release uses its product data.

Homebrew stays on the previous release until the public assets pass verification. The maintainer then runs the update helper, which checks public downloads before replacing the formula, and pushes it. CI never pushes tap history.

Rehearse for dry runs, changes to release machinery, or credential recovery. A rehearsal checks a temporary formula without changing the public tap, registries or GitHub Release. An unchanged normal release needs no rehearsal.

Recovery must establish the state of each channel separately: an authorization error or an existing version on one registry says nothing about the others. Publish only the verified original artifacts, and never skip a failed channel or call partial publication complete. Verify an existing GitHub Release without overwriting its notes or assets. Missing or mismatched downloads block the Homebrew update; repair missing assets only from the verified build, and investigate mismatches without replacing public files. Released assets never change, because published URLs and checksums depend on them.

## Delivery checks

A release is complete only with fresh install and runtime evidence from every official channel:

- Linux: npm and crates.io, plus PyPI when the release declares Linux wheels.
- macOS: PyPI and Homebrew.
- Go: clean-project install and read/write with the tagged Go module and matching SDK.

`post-release-smoke.yml` runs the registry and Homebrew checks. Its npm, PyPI and crates.io checks retry during registry propagation. Its Python jobs exercise the published wheels with CPython 3.11. Run the separate Go delivery smoke after publishing the Go tag. Build-time package checks do not replace public-delivery checks.

## Versioning policy

Plasmite follows semantic versioning as one product. The CLI, Rust crate, Python and Node packages, Go module and C library release together. A change that breaks documented behavior of any of them needs a new major version. Keeping one version means users never have to work out which binding matches which CLI.

### Version mapping

These must hold the same version in every release:

- `Cargo.toml [package].version`
- `bindings/python/pyproject.toml [project].version`
- `bindings/node/package.json .version`
- `bindings/node/package-lock.json .version` and `.packages[""].version`
- `bindings/node/native/Cargo.toml [package].version`

Run `scripts/bump_version.sh <version>` to change them together, then `just check`, which fails when any of them drift (`scripts/check-version-alignment.sh`). Commit the manifests together. Tags use `vX.Y.Z`. A move away from lockstep versions must update this document and the alignment script in the same change.

### Native library major version

The C library's major version equals the product's major version. `PLSM_ABI_MAJOR` in `include/plasmite.h` and `plsm_abi_major()` in `src/abi.rs` report it, and the shared library carries it in its file name: `libplasmite.so.2`, `libplasmite.2.dylib` and `plasmite2.dll`. Different majors can therefore sit side by side, and Python, Go and the Node add-on refuse to load a library with the wrong major. A new major release changes the constant, the file names in `scripts/stage_versioned_library.sh` and the packaging scripts, the pkg-config link line, and each binding's check together. The alignment script does not check these.

### Go module

The Go module path is `github.com/sandover/plasmite/bindings/go/v2`. Create a `bindings/go/vX.Y.Z` tag at the same commit as the product's `vX.Y.Z` tag. Push the Go tag once users can download the matching native SDK. From a clean project, the Go delivery smoke downloads that version and runs a round trip against the installed SDK. Use `bash scripts/go_module_smoke.sh vX.Y.Z /path/to/sdk`.

### Build identity

The CLI flags, the `version` command and MCP server metadata all report one build identity. A clean checkout at `v<package-version>` reports the bare package version. Other source builds report `<package-version>-dev+g<commit>`, with `.dirty` when tracked files have local changes; the package version supplies the base and does not choose the next release number. Source archives without Git metadata report `-dev+unknown`. Cargo registry packages report their package version.

The release profile controls optimization only; it does not mark a build as an official release. Cargo watches tracked files and Git state, so build identity refreshes when source, commits or tags change. Like Git, `.dirty` ignores untracked files: stage a new file to include it.
