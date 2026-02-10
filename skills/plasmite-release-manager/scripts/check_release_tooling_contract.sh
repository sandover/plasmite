#!/usr/bin/env bash
# Purpose: Enforce release-script tooling compatibility before tag/publish actions.
# Key outputs: Non-zero exit when release script tooling and workflow provisioning disagree.
# Role: Catch CI runner/tool mismatches early (for example rg usage without ripgrep install).
# Invariants: Release scripts using rg must either provide local fallback or require workflow install.
# Invariants: Workflow-level ripgrep provisioning satisfies scripts that intentionally depend on rg.
# Notes: Static check; does not execute release scripts or mutate repository state.

set -euo pipefail

workflow_file=".github/workflows/release.yml"

release_scripts=(
  "scripts/node_pack_smoke.sh"
  "scripts/python_wheel_smoke.sh"
  "scripts/cross_artifact_smoke.sh"
  "skills/plasmite-release-manager/scripts/verify_licensing_notices.sh"
)

if [[ ! -f "$workflow_file" ]]; then
  echo "error: missing workflow file: $workflow_file" >&2
  exit 2
fi

workflow_installs_ripgrep=0
if grep -Eq "ripgrep|apt-get install .*ripgrep|brew install ripgrep|choco install ripgrep" "$workflow_file"; then
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

if [[ ${#missing_contract[@]} -gt 0 ]]; then
  echo "error: release tooling contract failed" >&2
  echo "details: scripts use rg without fallback and workflow does not provision ripgrep:" >&2
  printf '  - %s\n' "${missing_contract[@]}" >&2
  echo "hint: add script fallback or install ripgrep in $workflow_file" >&2
  exit 1
fi

echo "ok: release tooling contract is satisfied"
