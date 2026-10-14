# Releasing Plasmite

The [release runbook](../../skills/plasmite-release-manager/SKILL.md) owns commands,
sequencing and recovery. This document owns release and compatibility policy;
it deliberately does not repeat the checklist.

## Release contract

A release ships one reviewed commit through GitHub SDK archives, crates.io, npm,
PyPI and Homebrew. Versions agree across the product's distribution surfaces.
`release/targets.json` owns native target/channel identity; the
[distribution guide](distribution.md) describes support tiers and prerequisites.

- Honor the requested review boundary and any publication hold.
- Require the canonical `just release-gate`, a current audit, and successful
  `ci.yml` for the exact release SHA before tagging.
- Run a local benchmark comparison for hot-path or storage changes. Compare with
  the previous release on the same host; routine patches need no benchmark.
- Commit and push the complete candidate before tagging. Never move a release tag.
- Track failed gates under `Release blockers: <release_target>` in Ergo.
  Failed or incomplete required gates block progression.
- Record target, source SHA, build/publish run IDs and evidence so recovery resumes
  the intended release instead of whichever workflow ran most recently.

## Build and publication

`release.yml` builds the tagged commit, smoke-tests every matrix target and stores
SDKs, Python distributions, the npm package and source metadata as artifacts.
Release builds pin Rust 1.88.0. A successful build is not publication.

`release-publish.yml` consumes a successful build matching the release tag and
source SHA. It checks artifacts and registry readiness, then verifies the
independently maintained Homebrew formula's version, URLs and checksums **before**
registry publication. CI verifies the tap; it does not push tap changes.

The maintainer updates the tap locally for a live release. Registries publish in
parallel after that gate; the GitHub Release is created only after all succeed.
**Remaining publication-order issue:** this order creates a Homebrew download
gap until release assets are public. The runbook documents recovery; it does not
close that gap. If publishing stalls, restore the previous formula until retry.

Rehearse for dry runs, changes to release machinery, or credential recovery.
Rehearsal verifies a temporary expected formula without changing the public tap,
registries or GitHub Release. A dry run ends with rehearsal evidence. An unchanged
normal live release needs no extra rehearsal.

Recovery must establish the exact state of each channel. Authorization errors and
an existing version on one registry do not prove the whole release succeeded.
Use the verified original artifacts; never bypass a failed channel or treat
partial publication as complete.

## Delivery and support tiers

Release completion requires fresh install/runtime evidence from all four channels:

- Linux: npm and crates.io, plus PyPI when the released manifest declares Linux wheels.
- macOS: PyPI and Homebrew, using a wheel supported by the selected release.

Select Python from the release's published wheel tags and `Requires-Python`.
The workflow tests CPython 3.11 to cover both 1.2.1's `cp311` wheels and newer
`py3-none` wheels; that test interpreter is not the package's support policy.

`post-release-smoke.yml` runs these checks. The local script handles registry
channels only; Homebrew is a separate macOS check. Retries are bounded and cover
registry propagation, not unsupported platforms or missing SDK prerequisites.
Build-time package checks do not replace public-delivery verification.

A platform/channel combination is official only with documented prerequisites
and passing automated install/runtime checks. Preview artifacts do not promote
themselves: promotion needs a manifest/support-policy change and delivery evidence.
Linux ARM64/ARMv7 SDKs and cargo-binstall remain preview; see the distribution guide.

## Versioning Policy

### Scope

Plasmite ships one product line with multiple distribution surfaces:

- Rust crate + CLI (`Cargo.toml`)
- Python package (`bindings/python/pyproject.toml`)
- Node package (`bindings/node/package.json`)
- Node native crate (`bindings/node/native/Cargo.toml`)

### Compatibility Definition

- **CLI compatibility**: stable command behavior follows semantic versioning intent; breaking CLI behavior bumps major. One narrow correction shipped in 1.1.0: removal of the remote MCP bridge added in 1.0.0. The approved 1.2.0 release adds a breaking CLI cutover to complete-message pipes. These exceptions do not change the policy for future releases.
- **Bindings compatibility**: Python/Node API behavior tracks the same semantic version as CLI releases.
- **`libplasmite` ABI compatibility**: ABI changes are treated as release-significant and share the same version bump policy as CLI/bindings.

Until explicitly revised, compatibility is managed in one lockstep release train.

### Version Mapping (Lockstep)

For every release, the following versions must be identical:

- `Cargo.toml [package].version`
- `bindings/python/pyproject.toml [project].version`
- `bindings/node/package.json .version`
- `bindings/node/package-lock.json .version`
- `bindings/node/package-lock.json .packages[""].version`
- `bindings/node/native/Cargo.toml [package].version`

### Required Workflow

1. Run `scripts/bump_version.sh <version>` to apply one version everywhere.
2. Run `just check` to verify version alignment alongside formatting, linting, and tests.
3. Commit all manifest updates together.

### Guardrails

- Tags use `vX.Y.Z` format.
- CI/local checks must fail when any mapped version drifts.
- Do not manually edit one manifest in isolation for release bumps.
- If policy changes away from lockstep, update this document and `scripts/check-version-alignment.sh` in the same change.

### Build identity

Every product-version surface reports one build identity: CLI flags, the
`version` command, and Model Context Protocol (MCP) server metadata. A clean
Git checkout at `v<package-version>` reports the bare package version. Other
source builds report `<package-version>-dev+g<commit>`, with `.dirty` when
tracked files have local changes. The package version supplies the base; this does not
select the next release number. Source archives without Git provenance report
`-dev+unknown`. Cargo registry packages retain their package version.

The compiler's release profile controls optimization; it does not mark a
build as an official release.

Cargo watches tracked files and Git state to refresh build identity when
source, commits, or tags change. Unchanged builds reuse their artifacts.
The `.dirty` marker follows Git's tracked-file convention: stage new files
to include them in the build identity.
