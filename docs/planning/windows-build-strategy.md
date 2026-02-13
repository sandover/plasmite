# Windows build strategy (Lite3 compiler path)

## Goal
Pick the lowest-risk compiler path for Windows Lite3 compilation and validate it empirically on GitHub Actions `windows-latest`.

## Candidate strategies
- `msvc-shim`: keep `cl.exe`, add compatibility shims/macros for GCC-style builtins/attributes.
- `clang-cl`: set `CC_x86_64_pc_windows_msvc=clang-cl` for C compilation while keeping the MSVC target/linker path.

## Probe workflow
- Workflow: `.github/workflows/windows-probe.yml`
- Dispatch command: `gh workflow run windows-probe.yml --ref main`
- Latest evidence run (post workflow fixes): [22000655822](https://github.com/sandover/plasmite/actions/runs/22000655822)
  - `probe (msvc-shim)` job: [63572163384](https://github.com/sandover/plasmite/actions/runs/22000655822/job/63572163384)
  - `probe (clang-cl)` job: [63572163411](https://github.com/sandover/plasmite/actions/runs/22000655822/job/63572163411)
- Downloaded probe artifacts:
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
- Result: **fail**, but **past Lite3 C-compile class of errors**.
- C compilation proceeds; failure occurs later in Rust core on Windows-incompatible POSIX semaphore usage:
  - `src/core/notify.rs` unresolved `libc::sem_*`, `mode_t`, `S_IRUSR`, `S_IWUSR`
- Additional warning: `-std=c11` is ignored by `clang-cl` in current invocation.
- Conclusion: compiler path is materially better than `msvc-shim`; next blocker is unrelated Windows runtime portability in notify backend.

## Decision
Default Windows C strategy: **`clang-cl`**.

Rationale:
- Lowest immediate complexity versus expanding/maintaining a custom MSVC compatibility shim.
- Empirically advances build farther than `msvc-shim` and isolates next blocker to Rust Windows support.
- Reversible and easy to scope in CI/workflow probes.

Fallback:
- Keep `msvc-shim` as fallback only if `clang-cl` becomes unavailable or introduces regressions after Windows runtime portability is addressed.

## Current blocker to full success gate
The validation gate requiring a successful `cargo build --release --bins` on `windows-latest` is **not yet met**.

Blocking issue:
- Windows notify implementation in `src/core/notify.rs` currently assumes POSIX semaphore symbols not provided on Windows.

## Next step
- Address Windows notify portability (stub or Windows-backed implementation) so `cargo build --release --bins` can complete under the selected `clang-cl` strategy.
