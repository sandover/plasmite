#!/usr/bin/env bash
# Purpose: Enforce release automation contract invariants before tag/publish actions.
# Key outputs: Non-zero exit when workflows/scripts drift from release contract expectations.
# Role: Catch release-tooling, provenance, and gating regressions before live releases.
# Invariants: Release smokes must use shared SDK normalization helper and safe archive checks.
# Invariants: Split workflows must keep fail-closed publish/release dependencies.
# Notes: Static check; does not execute release workflows or mutate repository state.

set -euo pipefail

workflow_build_file=".github/workflows/release.yml"
workflow_publish_file=".github/workflows/release-publish.yml"
sdk_helper_file="scripts/normalize_sdk_layout.sh"
node_smoke_file="scripts/node_pack_smoke.sh"
python_smoke_file="scripts/python_wheel_smoke.sh"
brew_verify_file="scripts/verify_homebrew_formula_alignment.sh"

release_scripts=(
  "$node_smoke_file"
  "$python_smoke_file"
  "scripts/cross_artifact_smoke.sh"
  "skills/plasmite-release-manager/scripts/verify_licensing_notices.sh"
)

if [[ ! -f "$workflow_build_file" ]]; then
  echo "error: missing workflow file: $workflow_build_file" >&2
  exit 2
fi
if [[ ! -f "$workflow_publish_file" ]]; then
  echo "error: missing workflow file: $workflow_publish_file" >&2
  exit 2
fi
if [[ ! -f "$sdk_helper_file" ]]; then
  echo "error: missing shared SDK helper: $sdk_helper_file" >&2
  exit 2
fi
if [[ ! -f "$brew_verify_file" ]]; then
  echo "error: missing Homebrew alignment checker: $brew_verify_file" >&2
  exit 2
fi

workflow_installs_ripgrep=0
if grep -Eq "ripgrep|apt-get install .*ripgrep|brew install ripgrep|choco install ripgrep" "$workflow_build_file" "$workflow_publish_file"; then
  workflow_installs_ripgrep=1
fi

missing_contract=()
for script in "${release_scripts[@]}"; do
  if [[ ! -f "$script" ]]; then
    continue
  fi

  if ! grep -Eq "(^|[^[:alnum:]_])rg([[:space:]]|$)" "$script"; then
    continue
  fi

  script_has_fallback=0
  if grep -Eq "command -v rg|has_rg|fallback" "$script"; then
    script_has_fallback=1
  fi

  if [[ "$script_has_fallback" -eq 0 && "$workflow_installs_ripgrep" -eq 0 ]]; then
    missing_contract+=("$script")
  fi
done

if ! grep -Fq 'source "$ROOT/scripts/normalize_sdk_layout.sh"' "$node_smoke_file"; then
  echo "error: node smoke must source shared SDK helper ($sdk_helper_file)." >&2
  exit 1
fi
if ! grep -Fq 'source "$ROOT/scripts/normalize_sdk_layout.sh"' "$python_smoke_file"; then
  echo "error: python smoke must source shared SDK helper ($sdk_helper_file)." >&2
  exit 1
fi
if ! grep -Eq "plasmite_normalize_sdk_dir" "$node_smoke_file"; then
  echo "error: node smoke must resolve SDK path via plasmite_normalize_sdk_dir." >&2
  exit 1
fi
if ! grep -Eq "plasmite_normalize_sdk_dir" "$python_smoke_file"; then
  echo "error: python smoke must resolve SDK path via plasmite_normalize_sdk_dir." >&2
  exit 1
fi

if ! grep -Eq "workflow_run:" "$workflow_publish_file"; then
  echo "error: release-publish workflow must support workflow_run trigger from release build." >&2
  exit 1
fi
if ! grep -Eq "workflow_dispatch:" "$workflow_publish_file"; then
  echo "error: release-publish workflow must support workflow_dispatch publish-only reruns." >&2
  exit 1
fi
if ! grep -Eq "build_run_id:" "$workflow_publish_file"; then
  echo "error: release-publish workflow_dispatch must require build_run_id input." >&2
  exit 1
fi
if ! grep -Eq "rehearsal:" "$workflow_publish_file"; then
  echo "error: release-publish workflow_dispatch must support rehearsal mode." >&2
  exit 1
fi
if ! grep -Eq "github\\.event_name != 'workflow_run' \\|\\| github\\.event\\.workflow_run\\.conclusion == 'success'" "$workflow_publish_file"; then
  echo "error: release-publish must skip workflow_run invocations when source release build failed." >&2
  exit 1
fi

checkout_ref_count="$(grep -Fc 'ref: ${{ needs.release-context.outputs.tag }}' "$workflow_build_file")"
if [[ "$checkout_ref_count" -lt 3 ]]; then
  echo "error: release build jobs must checkout the release tag from release-context outputs." >&2
  exit 1
fi
if ! grep -Fq "requested tag '\${tag}' does not exist on origin." "$workflow_build_file"; then
  echo "error: release build workflow must fail fast when manual tag input does not exist remotely." >&2
  exit 1
fi

if ! grep -Eq "publish-preflight:" "$workflow_publish_file"; then
  echo "error: release-publish workflow missing publish-preflight job." >&2
  exit 1
fi
for required_msg in \
  "NPM_TOKEN not set for publish-preflight." \
  "PYPI_API_TOKEN not set for publish-preflight." \
  "CARGO_REGISTRY_TOKEN not set for publish-preflight." \
  "bypass-2FA"; do
  if ! grep -Fq "$required_msg" "$workflow_publish_file"; then
    echo "error: publish-preflight diagnostics missing expected guidance: $required_msg" >&2
    exit 1
  fi
done

if ! grep -Eq "gh run download .*release-metadata" "$workflow_publish_file"; then
  echo "error: release-publish must download release-metadata to verify artifact provenance." >&2
  exit 1
fi
if ! grep -Eq "workflowName.*release|belongs to workflow .*release" "$workflow_publish_file"; then
  echo "error: release-publish must verify build_run_id belongs to release build workflow." >&2
  exit 1
fi
if ! grep -Eq "verify-homebrew-tap:" "$workflow_publish_file"; then
  echo "error: release-publish must include verify-homebrew-tap job." >&2
  exit 1
fi
if ! grep -Eq "verify_homebrew_formula_alignment\\.sh" "$workflow_publish_file"; then
  echo "error: release-publish must verify Homebrew formula alignment before publish." >&2
  exit 1
fi

if ! grep -Eq "needs: \\[resolve-build-run, publish-preflight, collect-build-artifacts, verify-homebrew-tap, publish-pypi, publish-crates-io, publish-npm\\]" "$workflow_publish_file"; then
  echo "error: final release job must remain fail-closed on preflight, provenance, Homebrew alignment, and publish jobs." >&2
  exit 1
fi

publish_needs_count="$(grep -Fc "needs: [resolve-build-run, publish-preflight, collect-build-artifacts, verify-homebrew-tap]" "$workflow_publish_file")"
if [[ "$publish_needs_count" -lt 3 ]]; then
  echo "error: publish jobs must depend on verify-homebrew-tap." >&2
  exit 1
fi

if ! grep -Eq "rehearsal == 'true'|rehearsal != 'true'" "$workflow_publish_file"; then
  echo "error: release-publish must guard publish/release actions with rehearsal mode switches." >&2
  exit 1
fi

if [[ ${#missing_contract[@]} -gt 0 ]]; then
  echo "error: release tooling contract failed" >&2
  echo "details: scripts use rg without fallback and workflows do not provision ripgrep:" >&2
  printf '  - %s\n' "${missing_contract[@]}" >&2
  echo "hint: add script fallback or install ripgrep in release workflow files" >&2
  exit 1
fi

echo "ok: release tooling contract is satisfied"
