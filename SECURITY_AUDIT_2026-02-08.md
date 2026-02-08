# Security & Dependency Risk Audit (2026-02-08)

## Scope

This audit covers dependency and supply-chain risk for:

- Rust (`Cargo.toml`, `Cargo.lock`)
- Node (`bindings/node/package.json`, `bindings/node/package-lock.json`)
- Python (`bindings/python/pyproject.toml`)
- Go (`bindings/go/go.mod`)
- Native/C vendored code (`build.rs`, `vendor/lite3/**`, `c/**`)

## Method

- Reviewed manifests and lockfiles.
- Enumerated resolved Rust direct dependencies with `cargo tree --depth 1`.
- Enumerated Node lockfile package set.
- Attempted ecosystem-specific advisory checks (`cargo audit`, `npm audit`).

## High-Severity Findings

### 1) Runtime option permits plaintext remote writes (`--insecure-no-tls`) when bound non-loopback

**Severity:** High (configuration-driven, but explicit security downgrade)

The server supports `--allow-remote` + `--insecure-no-tls`, which permits non-loopback write access without TLS if operator chooses this mode. This creates confidentiality and token replay/exposure risk on untrusted networks. The code does enforce guardrails and marks this mode unsafe, but the footgun exists by design.

- CLI flag and help text exposes this insecure mode.
- Validation logic permits remote writes without TLS only when `insecure_no_tls` is set.

**Mitigations:**

- Keep default secure behavior (already present).
- Add a second explicit confirmation control for production (e.g., env var `PLASMITE_ALLOW_INSECURE_NO_TLS=1`) before accepting this flag.
- Emit high-visibility startup warning + structured log field `security_mode=insecure-no-tls`.
- Consider build/profile gate to disable this mode in release artifacts for managed deployments.

**Compatibility impact:**

- Additional confirmation gate is a breaking operational change for current insecure deployments, but low-risk and security-positive.

### 2) Vendored native C stack lacks explicit upstream version provenance and automated CVE tracking

**Severity:** High (supply-chain visibility gap in memory-unsafe surface)

The core build compiles vendored C sources (`vendor/lite3`, embedded `yyjson`, `nibble_base64`) directly via `cc`. There is no manifest in-repo mapping vendored snapshots to upstream tags/commits, no checksum policy, and no automated advisory feed for these components.

Because this code is memory-unsafe language surface in the trusted computing base, provenance opacity increases latent high-severity risk.

**Mitigations:**

- Add `vendor/THIRD_PARTY_VERSIONS.md` with exact upstream commit/tag + retrieval URL + date + local patch notes.
- Add hash verification (e.g., script/check in CI) for vendored tarballs or directory checksums.
- Add periodic review task for vendored C dependencies and an owner.
- Prefer dynamically linking to distro-maintained libraries where practical (tradeoff: portability vs patch velocity).

**Compatibility impact:**

- Documentation/checksum additions are non-breaking.
- Linking-strategy changes can alter build portability and deployment assumptions.

## Medium-Risk Findings

### 3) Advisory scanning is not currently runnable in this environment; no evidence of in-repo fallback

- `cargo audit` is not preinstalled and install failed due network policy.
- `npm audit` failed with registry advisory endpoint 403.

Without successful advisory queries (or mirrored offline DB), this audit cannot cryptographically confirm “no known CVEs” status for current lockfiles.

**Mitigations:**

- Add CI jobs for `cargo audit` and `npm audit --production` in a network-allowed runner.
- Keep a mirrored advisory source (or SARIF ingest from CI) to preserve attestable results.

## Dependency/Versioning Risk Review by Ecosystem

### Rust

Resolved direct dependencies are generally current-generation ecosystem versions (e.g., `axum 0.7.9`, `tokio 1.49.0`, `rustls 0.23.36`, `hyper 1.8.1`).

**Risks noted:**

- `rcgen 0.12.1` is relatively old compared with newer major lines; certificate-generation paths deserve periodic review.
- Several dependencies are specified with broad semver ranges in `Cargo.toml` (`"4"`, `"1"`, `"2"`), relying on lockfile discipline for determinism.

**Mitigations:**

- Continue committing `Cargo.lock` for applications.
- Add scheduled dependency update workflow (weekly/monthly) plus regression tests.
- Evaluate `cargo-deny` for license/advisory/source policy enforcement.

### Node

Node package scope is minimal and mostly development tooling (`@napi-rs/cli`, `typescript`, `@types/node`, `undici-types`). Runtime dependency surface appears intentionally tiny.

**Risks noted:**

- Caret ranges in devDependencies allow unattended minor/patch drift.
- No successful advisory resolution in this environment.

**Mitigations:**

- Use Renovate/Dependabot with grouped updates and CI gates.
- Optionally pin exact versions for release engineering reproducibility.

### Python

`pyproject.toml` declares no third-party runtime dependencies beyond setuptools build backend.

**Risk posture:** Low dependency CVE exposure for Python layer itself.

**Residual risk:** Native binary loading from bundled artifacts still inherits Rust/C supply-chain posture.

### Go

`go.mod` has no external module requirements.

**Risk posture:** Low third-party dependency risk.

**Residual risk:** Wrapper transitively trusts local native library behavior.

### Native/C

Build compiles local shim + vendored C sources with C2x/GNU2x flags. No explicit hardening flags are configured in `build.rs` (stack protector/fortify/sanitizer flags are toolchain-default-dependent).

**Mitigations:**

- Evaluate explicit hardening flags for release builds where toolchain/platform permits.
- Add ASan/UBSan CI profile for vendored C surfaces.

## Recommended Upgrade/Mitigation Plan

1. **Immediate (this sprint)**
   - Add provenance + checksum docs for vendored C deps.
   - Add CI advisory checks (`cargo audit`, `npm audit`) on networked runner.
   - Add startup warning/telemetry hardening for insecure server mode.

2. **Near-term (1–2 sprints)**
   - Add `cargo-deny` policy.
   - Introduce scheduled dependency update bot workflow.
   - Add sanitizer CI path for native components.

3. **Ongoing**
   - Monthly dependency refresh and changelog review.
   - Re-run full audit on every release candidate.

## Evidence Commands Executed

- `cargo tree --depth 1`
- `cargo tree | rg "openssl|native-tls|ring|rustls-webpki|webpki|hyper|h2|tokio"`
- `go list -m all` (in `bindings/go`)
- `node -e "const l=require('./bindings/node/package-lock.json'); const pk=l.packages||{}; for (const [k,v] of Object.entries(pk)) if(k&&v.version) console.log((k||'.')+': '+v.version);"`
- `cargo audit -q` (failed: command missing)
- `cargo install cargo-audit` (failed: registry access 403)
- `npm audit --prefix bindings/node --audit-level=high` (failed: advisory endpoint 403)

