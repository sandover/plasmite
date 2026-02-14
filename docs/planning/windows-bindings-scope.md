# Windows bindings scope (Python + Node)

## Purpose
Decide whether Windows no-compile support should extend from CLI preview artifacts into Python and Node channels, and at what rollout level.

## Current baseline (as of v0.1.14 preview work)

- CLI preview artifacts exist for `x86_64-pc-windows-msvc` via GitHub release assets:
  - `plasmite_<version>_windows_amd64_preview.zip`
  - matching `.sha256`
- CI reality: macOS is primary dev host; Windows feedback loop is GitHub Actions.
- Known risk: local Windows write-path smoke currently reports `failed to encode json as lite3` in preview CI roundtrip attempts.

## Channel feasibility

| Channel | Immediate feasibility | Why | Recommendation |
| --- | --- | --- | --- |
| Python wheel (`win_amd64`) | Medium technical effort, high validation risk right now | Packaging/runtime code currently assumes `libplasmite.(dylib|so)` + `plasmite` (no `.dll` / `.exe` paths); write-path reliability on Windows is not yet stable | **Preview-only later** (after Windows runtime write-path stabilization) |
| Node package (`win32-x64`) | Medium-high technical effort, high validation risk right now | Platform maps and package files exclude Windows; native staging expects `.so/.dylib` and non-`.exe` CLI; release matrix has no Windows node-native job | **Defer native Windows packaging**; keep remote-only JS API usable |

## Python (`win_amd64`) detail

### Code/package constraints today

- `bindings/python/setup.py` bundles only:
  - `libplasmite.dylib`
  - `libplasmite.so`
  - `libplasmite.a`
  - `plasmite` (no `.exe`)
- `bindings/python/plasmite/__init__.py` loader searches only `.dylib`/`.so` names.
- `bindings/python/plasmite/_cli.py` expects bundled `plasmite` filename, not `plasmite.exe`.
- `scripts/python_wheel_smoke.sh` validates wheel members for `.dylib|.so` + `plasmite`.
- `release.yml` Python wheel matrix currently covers Linux/macOS only.

### Loader/runtime implications

- Windows wheel support requires explicit `.dll` and `.exe` handling in bundle + loader logic.
- Wheels must include the correct native runtime payload and pass import + CLI smoke on `windows-latest`.
- Even with packaging fixes, local append/write confidence is gated by unresolved Windows runtime behavior.

### Recommendation

- **Do not add Windows wheels to official release publish now.**
- Add as **preview-only** after runtime stabilization:
  1. Windows smoke roundtrip is consistently green.
  2. Python loader + CLI wrapper support `.dll`/`.exe` paths.
  3. Windows wheel smoke job is green across repeated runs.

## Node (`win32-x64`) detail

### Code/package constraints today

- `bindings/node/index.js` and `bindings/node/bin/plasmite.js` platform maps only include Linux/macOS.
- `bindings/node/package.json` `files` include only:
  - `native/linux-x64/**`, `native/linux-arm64/**`
  - `native/darwin-x64/**`, `native/darwin-arm64/**`
- `bindings/node/scripts/prepare_native_assets.js` and `scripts/package_node_natives.sh` expect `.so/.dylib` and `plasmite` (no `.dll` / `.exe`).
- `release.yml` node-native matrix excludes Windows.

### Runtime implications

- Native Windows packaging requires adding `win32-x64` platform mapping and staging rules for:
  - `index.node`
  - `plasmite.dll` (or loader-compatible equivalent)
  - `plasmite.exe`
- Node binding can still provide value on Windows in **remote-only mode** via `RemoteClient`, but native local mode is currently out-of-scope.

### Recommendation

- **Defer native Windows Node packaging** until after core Windows runtime hardening.
- Keep remote-only JS usage documented for Windows in the interim.

## CI / validation cost estimate

To ship either channel with confidence, minimum recurring CI additions:

1. Windows packaging build job(s) for channel artifacts.
2. Windows install/runtime smoke for packaged artifact:
   - Python: wheel install, import, local operation + CLI check.
   - Node: npm pack/install, native load, local operation + CLI check.
3. Release workflow integration for uploading/publishing Windows channel artifacts.
4. Failure triage loop for flaky Windows regressions (longer due single remote builder).

Estimated incremental cost:
- Additional Windows matrix jobs per PR/release.
- Meaningful increase in CI time and failure surface area.

## Decision summary

- Python (`win_amd64`): **Preview-only later** (not yet official).
- Node (`win32-x64` native): **Defer**; keep remote-only usage path.
- Re-evaluate after Windows smoke/write-path reliability reaches promotion criteria.
