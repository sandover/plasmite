# Pre-Release QA Gates

For each gate:
- collect evidence
- decide pass/fail
- if fail or incomplete: file blocker task with `scripts/file_release_blocker.sh` (or `scripts/file_release_blocker_with_evidence.sh` for workflow failures)

Use `release_target` and `base_tag` consistently.

## Runtime Preflight (Before Gate 1)

Required for trustworthy release evidence:
- network access to GitHub and package registries
- working `gh` host auth (`gh auth status`, `gh api user -q .login`)
- writable local scratch/cache directories (use `.scratch/`)

Preflight commands:
- `mkdir -p .scratch .scratch/release`
- `test -w .scratch`
- `bash skills/plasmite-release-manager/scripts/check_release_tooling_contract.sh`

Clean-filesystem guard (required before release mechanics):
- verify release helper scripts do not rely on pre-existing `.scratch` paths
- if any script fails with `mktemp` / `mkdtemp` "No such file or directory", treat as release-blocking workflow defect

If runtime preflight fails, block release and file a blocker task before running gate checks.

## 0) CI Tooling Compatibility Contract

Goal:
- prevent release workflow failures caused by runner tool mismatch (for example, scripts using `rg` when runners do not provide ripgrep)

Evidence commands:
- `bash skills/plasmite-release-manager/scripts/check_release_tooling_contract.sh`
- `rg -n "ripgrep|rg " .github/workflows/release.yml`

Block if:
- release script tooling requirements are neither guarded by script fallback nor provisioned in workflow
- the tooling contract check cannot run or returns non-zero

## 1) Dependency & Vulnerability Monitoring

Evidence commands:
- `cargo audit --db .scratch/advisory-db --no-fetch --ignore yanked` (or `just audit` if available)
- `cd bindings/node && npm audit --omit=dev`
- `cd bindings/python && python3 -m pip list --format=json` (record dependency snapshot)
- `cd bindings/go && go list -m -u all`

Block if:
- high/critical vulnerability exists in shipped dependency graph
- likely supply-chain concern is unresolved
- dependency risk cannot be assessed with available tooling

## 2) Memory Safety & Unsafe Boundaries

Evidence commands:
- `rg -n "\\bunsafe\\b|extern \\\"C\\\"|ctypes|N-API|cgo|ffi" src bindings include c`
- `cargo test -q`

Review focus:
- ownership/lifetime boundaries across ABI and language bindings
- pointer/nullability contracts and buffer length invariants

Block if:
- any safety invariant is unclear, untested, or likely violated

## 3) Concurrency Correctness & Crash Consistency

Evidence commands:
- `cargo test -q core::pool::tests::multi_writer_stress`
- `cargo test -q core::pool::tests::crash_append_phases_preserve_invariants`
- `cargo test -q tests::lock_smoke::concurrent_poke_is_serialized` (or `cargo test -q --test lock_smoke`)

Review focus:
- writer lock invariants
- partial-write/torn-state handling
- recovery behavior under crash windows

Block if:
- race window or crash window can violate on-disk invariants

## 5) Performance Regression Guard

Evidence commands:
- `git diff --name-only <base_tag>..HEAD`
- `cargo build --release --example plasmite-bench`
- `./target/release/examples/plasmite-bench --format json > .scratch/release/bench-current.json`

Optional stronger comparison:
- check out `<base_tag>` in a detached worktree and capture baseline with same host/settings

Comparison policy:
- use same host and power mode for baseline/current
- collect at least 3 runs per scenario and compare median `ms_per_msg`
- ignore scenarios where both medians are below `0.0001 ms/msg` (timer quantization noise)

Block if:
- median regression >= 15% in core scenarios (append, multi_writer, get_scan) without approved explanation
- no trustworthy comparison could be produced

## 6) API/CLI Stability & Compatibility

Evidence commands:
- `cargo test -q --test cli_integration`
- `cargo test -q --test remote_integration`
- `cargo test -q`

Review focus:
- CLI flags/help/exit behavior drift
- Rust API compatibility expectations
- binding semantics parity with core behavior

Block if:
- breaking drift is found without explicit release-note justification

## 7) Documentation Alignment (Docs Match Reality)

Evidence commands:
- `rg -n "install|quickstart|example|version|release|npm|pip|uv|brew|go get" README.md docs bindings`
- run representative quickstart snippets where feasible

Block if:
- install/docs examples no longer match actual behavior or outputs

## 8) Binding Parity & Packaging Health

Evidence commands:
- `just bindings-test`
- `bash scripts/node_pack_smoke.sh`
- `bash scripts/python_wheel_smoke.sh`
- `just cross-artifact-smoke`

Notes:
- Python wheel smoke requires package-index access (or preinstalled build dependencies) to install the `build` backend.
- Treat package-index/network failures as environment blockers, not as binding regressions.

Block if:
- behavior differs materially between bindings and core
- packaging/install smoke fails for supported channels

## 9) Server / Web UI Security Review

Evidence commands:
- `cargo test -q --test remote_integration`
- `rg -n "token|auth|tls|insecure|allow-non-loopback|body|timeout|limit" src spec docs`

Review focus:
- authn/authz assumptions
- TLS defaults and insecure-mode ergonomics
- input validation and resource exhaustion controls

Block if:
- insecure defaults or exploitable gaps are identified without mitigation

## 11) Licensing & Notices

Evidence commands:
- `cargo metadata --format-version 1 > .scratch/release/cargo-metadata.json`
- `rg -n "License|MIT|Apache|GPL|BSD|MPL" THIRD_PARTY_NOTICES.md Cargo.toml bindings`
- `bash skills/plasmite-release-manager/scripts/verify_licensing_notices.sh`
- verify shipped notices match distributed artifacts

Block if:
- required attribution/notice is missing
- problematic license appears in release payload without policy decision
