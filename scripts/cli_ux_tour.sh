#!/usr/bin/env bash
# Purpose: Run a broad, repeatable tour of the plasmite CLI and capture output for UX review.
# Key exports: None; invoke as `scripts/cli_ux_tour.sh [log_path]`.
# Role: Exercise representative success and failure paths without touching default user state.
# Invariants: Uses an isolated `tmp/` workspace and explicit `--dir` for all pool operations.
# Invariants: Continues through expected failures, records per-command exit status, and cleans
# transient artifacts unless `KEEP_WORKDIR=1` is set.

set -u -o pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

LOG_PATH="${1:-tmp/cli-ux-tour.log}"
PLASMITE_BIN="${PLASMITE_BIN:-${REPO_ROOT}/target/debug/plasmite}"
KEEP_WORKDIR="${KEEP_WORKDIR:-0}"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"
WORK_DIR="${REPO_ROOT}/tmp/cli-ux-tour-${RUN_ID}"
POOL_DIR="${WORK_DIR}/pools"
INIT_DIR="${WORK_DIR}/serve-init"

POOL_MAIN="tour-main"
POOL_AUX="tour-aux"
POOL_PARTIAL="tour-partial"
MISSING_POOL="tour-missing"

FAILURES=0

cleanup() {
  if [[ "${KEEP_WORKDIR}" == "1" ]]; then
    return
  fi
  if [[ -d "${WORK_DIR}" ]]; then
    rm -rf "${WORK_DIR}"
  fi
}
trap cleanup EXIT

if [[ ! -x "${PLASMITE_BIN}" ]]; then
  printf 'Missing executable: %s\n' "${PLASMITE_BIN}" >&2
  printf 'Build first (for example: cargo build --bin plasmite)\n' >&2
  exit 1
fi

mkdir -p "$(dirname -- "${LOG_PATH}")" "${POOL_DIR}" "${INIT_DIR}"

run_plasmite() {
  "${PLASMITE_BIN}" "$@"
}

record_case() {
  local label="$1"
  local expected="$2"
  shift 2

  {
    printf '\n=== %s ===\n' "${label}"
    printf '$'
    printf ' %q' "$@"
    printf '\n'
  } >> "${LOG_PATH}"

  local output=""
  local status=0
  output="$("$@" 2>&1)" || status=$?

  if [[ -n "${output}" ]]; then
    printf '%s\n' "${output}" >> "${LOG_PATH}"
  fi

  printf '[exit=%s expected=%s]\n' "${status}" "${expected}" >> "${LOG_PATH}"

  local mismatch=0
  case "${expected}" in
    zero)
      if [[ "${status}" -ne 0 ]]; then
        mismatch=1
      fi
      ;;
    nonzero)
      if [[ "${status}" -eq 0 ]]; then
        mismatch=1
      fi
      ;;
    any)
      mismatch=0
      ;;
    *)
      printf '[invalid expectation: %s]\n' "${expected}" >> "${LOG_PATH}"
      mismatch=1
      ;;
  esac

  if [[ "${mismatch}" -eq 1 ]]; then
    FAILURES=$((FAILURES + 1))
    printf '[unexpected status for case: %s]\n' "${label}" >> "${LOG_PATH}"
  fi
}

cat > "${WORK_DIR}/good.json" << 'JSON'
{"kind":"file","ok":true}
JSON

cat > "${WORK_DIR}/mixed.jsonl" << 'JSONL'
{"kind":"jsonl","line":1}
not-json
{"kind":"jsonl","line":2}
JSONL

{
  printf '# plasmite CLI UX tour\n'
  printf 'started_utc: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'repo_root: %s\n' "${REPO_ROOT}"
  printf 'plasmite_bin: %s\n' "${PLASMITE_BIN}"
  printf 'work_dir: %s\n' "${WORK_DIR}"
  printf 'pool_dir: %s\n' "${POOL_DIR}"
} > "${LOG_PATH}"

record_case "top-level help" zero run_plasmite --help
record_case "pool help" zero run_plasmite pool --help
record_case "version" zero run_plasmite version

record_case "create pools" zero run_plasmite --dir "${POOL_DIR}" pool create "${POOL_MAIN}" "${POOL_AUX}" --size 1M
record_case "create duplicate pool" nonzero run_plasmite --dir "${POOL_DIR}" pool create "${POOL_MAIN}"
record_case "pool list (human)" zero run_plasmite --dir "${POOL_DIR}" pool list
record_case "pool list (json)" zero run_plasmite --dir "${POOL_DIR}" pool list --json
record_case "pool info (human)" zero run_plasmite --dir "${POOL_DIR}" pool info "${POOL_MAIN}"
record_case "pool info (json)" zero run_plasmite --dir "${POOL_DIR}" pool info "${POOL_MAIN}" --json
record_case "pool info missing" nonzero run_plasmite --dir "${POOL_DIR}" pool info "${MISSING_POOL}"

record_case "feed inline json with tag" zero run_plasmite --dir "${POOL_DIR}" feed "${POOL_MAIN}" '{"kind":"inline","ok":true}' --tag ux
record_case "feed from file" zero run_plasmite --dir "${POOL_DIR}" feed "${POOL_MAIN}" --file "${WORK_DIR}/good.json"
record_case "feed mixed jsonl with skip errors" any run_plasmite --dir "${POOL_DIR}" feed "${POOL_MAIN}" --file "${WORK_DIR}/mixed.jsonl" --in jsonl --errors skip
record_case "fetch existing seq" zero run_plasmite --dir "${POOL_DIR}" fetch "${POOL_MAIN}" 1
record_case "fetch missing seq" nonzero run_plasmite --dir "${POOL_DIR}" fetch "${POOL_MAIN}" 999999
record_case "follow tail one (jsonl)" zero run_plasmite --dir "${POOL_DIR}" follow "${POOL_MAIN}" --tail 2 --one --jsonl
record_case "follow timeout on quiet pool" nonzero run_plasmite --dir "${POOL_DIR}" follow "${POOL_AUX}" --timeout 300ms --one --jsonl

record_case "doctor one pool" zero run_plasmite --dir "${POOL_DIR}" doctor "${POOL_MAIN}"
record_case "doctor all pools json" zero run_plasmite --dir "${POOL_DIR}" doctor --all --json

record_case "serve check (human)" zero run_plasmite --dir "${POOL_DIR}" serve check
record_case "serve check (json)" zero run_plasmite --dir "${POOL_DIR}" serve check --json
record_case "serve init fresh dir" zero run_plasmite --dir "${POOL_DIR}" serve init --output-dir "${INIT_DIR}"
record_case "serve init without force on existing files" nonzero run_plasmite --dir "${POOL_DIR}" serve init --output-dir "${INIT_DIR}"
record_case "serve init with force" zero run_plasmite --dir "${POOL_DIR}" serve init --output-dir "${INIT_DIR}" --force

record_case "delete existing aux pool" zero run_plasmite --dir "${POOL_DIR}" pool delete "${POOL_AUX}"
record_case "delete missing pool" nonzero run_plasmite --dir "${POOL_DIR}" pool delete "${MISSING_POOL}"
record_case "create pool for partial delete case" zero run_plasmite --dir "${POOL_DIR}" pool create "${POOL_PARTIAL}"
record_case "delete mixed existing+missing" nonzero run_plasmite --dir "${POOL_DIR}" pool delete "${POOL_PARTIAL}" "${MISSING_POOL}"
record_case "delete main pool" zero run_plasmite --dir "${POOL_DIR}" pool delete "${POOL_MAIN}"
record_case "pool list after deletes" zero run_plasmite --dir "${POOL_DIR}" pool list --json
record_case "invalid subcommand UX" nonzero run_plasmite --dir "${POOL_DIR}" pool unknown-subcommand

{
  printf '\n=== summary ===\n'
  printf 'unexpected_status_cases: %s\n' "${FAILURES}"
  printf 'finished_utc: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if [[ "${KEEP_WORKDIR}" == "1" ]]; then
    printf 'work_dir_retained: %s\n' "${WORK_DIR}"
  else
    printf 'work_dir_retained: no\n'
  fi
} >> "${LOG_PATH}"

printf 'Wrote CLI UX tour log: %s\n' "${LOG_PATH}"
if [[ "${FAILURES}" -ne 0 ]]; then
  printf 'Tour completed with %s unexpected status mismatches.\n' "${FAILURES}" >&2
  exit 1
fi
