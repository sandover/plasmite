# Windows build strategy (Lite3 compiler path)

## Goal
Pick the lowest-risk compiler path for Windows Lite3 compilation and validate it empirically on GitHub Actions `windows-latest`.

## Candidate strategies
- `msvc-shim`: keep `cl.exe`, add compatibility shims/macros for GCC-style builtins/attributes.
- `clang-cl`: set `CC_x86_64_pc_windows_msvc=clang-cl` for C compilation while keeping the MSVC target/linker path.

## Probe workflow
- Workflow: `.github/workflows/windows-probe.yml`
- Dispatch command: `gh workflow run windows-probe.yml --ref main`
- Comparative run (both strategies): [22000655822](https://github.com/sandover/plasmite/actions/runs/22000655822)
  - `probe (msvc-shim)` job: [63572163384](https://github.com/sandover/plasmite/actions/runs/22000655822/job/63572163384)
  - `probe (clang-cl)` job: [63572163411](https://github.com/sandover/plasmite/actions/runs/22000655822/job/63572163411)
- Confirmation run (selected strategy only): [22001090359](https://github.com/sandover/plasmite/actions/runs/22001090359)
  - `probe (clang-cl)` job: [63573658728](https://github.com/sandover/plasmite/actions/runs/22001090359/job/63573658728)
- Downloaded comparative artifacts:
  - `.scratch/windows-probe-run22000655822/msvc-shim/cargo-build.log`
  - `.scratch/windows-probe-run22000655822/clang-cl/cargo-build.log`

## Empirical outcome
### `msvc-shim`
- Result: **fail** in Lite3 C compilation.
- Signature matches prior user reports:
  - missing GCC builtins (`__builtin_expect`)
  - parser fallout around `lite3.h` / `lite3_context_api.h`
  - `cc-rs` failure invoking `cl.exe` on `lite3_shim.c`
- Conclusion: this path still requires non-trivial shim work and remains high-risk.

### `clang-cl`
- Comparative run result: **fail**, but **past Lite3 C-compile class of errors**.
  - C compilation succeeds and build fails later in Rust core due to Windows-incompatible POSIX semaphore usage in `src/core/notify.rs`.
- Confirmation run result: **success**.
  - `cargo build --release --bins` completed (`Finished release profile ... in 4m 10s`).
  - Smoke gate passed: `target\\release\\plasmite.exe --version` => `plasmite 0.1.14`.
  - Artifact uploaded: `windows-probe-clang-cl` (artifact ID `5505265001`, SHA256 zip digest `09f7515efd2335eb9da0e5060cd14a4b1b8f3baa07d46453f8528a114f9adb38`).
- Conclusion: this is the only path with a successful Windows release-bin build and smoke run.

## Decision
Default Windows C strategy: **`clang-cl`**.

Rationale:
- Lowest immediate complexity versus expanding/maintaining a custom MSVC compatibility shim.
- Empirically demonstrated end-to-end `windows-latest` success (`cargo build --release --bins` + smoke) after the notify portability fix.
- Reversible and easy to scope in CI/workflow probes.

Fallback:
- Keep `msvc-shim` as fallback only if `clang-cl` becomes unavailable or introduces regressions after Windows runtime portability is addressed.

## Validation gate status
The required gate for this task is now met:
- successful `cargo build --release --bins` on `windows-latest` with selected strategy
- successful Windows smoke command (`plasmite.exe --version`)
- CI evidence captured in workflow/job logs and uploaded probe artifact
