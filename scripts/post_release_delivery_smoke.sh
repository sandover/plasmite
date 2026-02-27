#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: post_release_delivery_smoke.sh --version <X.Y.Z> [options]

Validate post-release install + minimal runtime behavior across delivery channels.

Options:
  --version <X.Y.Z>            Required release version (without leading v)
  --channels <csv>             Channels to check (default: npm,pypi,crates,homebrew)
  --max-wait-minutes <n>       Retry budget for propagation checks (default: 20)
  -h, --help                   Show this help

Channels:
  npm       npm + pnpm install/runtime check (pnpm best-effort when unavailable)
  pypi      uv install/runtime check
  crates    cargo install/runtime check
  homebrew  formula visibility check (advisory)

Environment:
  PLASMITE_KEEP_SCRATCH=1      Preserve scratch workdirs/logs under .scratch/
USAGE
}

VERSION=""
CHANNELS="npm,pypi,crates,homebrew"
MAX_WAIT_MINUTES=20

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    --channels)
      CHANNELS="${2:-}"
      shift 2
      ;;
    --max-wait-minutes)
      MAX_WAIT_MINUTES="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$VERSION" ]]; then
  echo "error: --version is required" >&2
  usage >&2
  exit 2
fi
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "error: --version must look like X.Y.Z (got '$VERSION')" >&2
  exit 2
fi
if ! [[ "$MAX_WAIT_MINUTES" =~ ^[0-9]+$ ]]; then
  echo "error: --max-wait-minutes must be an integer" >&2
  exit 2
fi

scratch_root=".scratch/post-release-smoke-$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$scratch_root"

cleanup() {
  if [[ "${PLASMITE_KEEP_SCRATCH:-0}" == "1" ]]; then
    echo "scratch preserved at: $scratch_root"
    return
  fi
  rm -rf "$scratch_root"
}
trap cleanup EXIT

deadline_epoch="$(( $(date +%s) + MAX_WAIT_MINUTES * 60 ))"

is_required_channel() {
  case "$1" in
    npm|pypi|crates)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

check_npm_channel() {
  if ! command -v npm >/dev/null 2>&1; then
    echo "[npm] npm is required" >&2
    return 1
  fi

  local npm_dir="$scratch_root/npm"
  rm -rf "$npm_dir"
  mkdir -p "$npm_dir"
  (
    cd "$npm_dir"
    npm init -y >/dev/null 2>&1
    npm install --silent --no-audit --no-fund "plasmite@$VERSION" >/dev/null
    ./node_modules/.bin/plasmite --version | grep -q "$VERSION"
  )

  local -a pnpm_cmd=()
  if command -v pnpm >/dev/null 2>&1; then
    pnpm_cmd=(pnpm)
  elif command -v corepack >/dev/null 2>&1; then
    pnpm_cmd=(corepack pnpm)
  else
    echo "[npm] warning: pnpm/corepack unavailable; skipping pnpm leg"
    return 0
  fi

  local pnpm_dir="$scratch_root/pnpm"
  rm -rf "$pnpm_dir"
  mkdir -p "$pnpm_dir"
  (
    cd "$pnpm_dir"
    npm init -y >/dev/null 2>&1
    "${pnpm_cmd[@]}" add "plasmite@$VERSION" >/dev/null
    ./node_modules/.bin/plasmite --version | grep -q "$VERSION"
  )
}

check_pypi_channel() {
  if ! command -v uv >/dev/null 2>&1; then
    echo "[pypi] uv is required" >&2
    return 1
  fi
  local uv_cache_dir="$scratch_root/uv-cache"
  mkdir -p "$uv_cache_dir"
  UV_CACHE_DIR="$uv_cache_dir" uv tool run --from "plasmite==$VERSION" plasmite --version | grep -q "$VERSION"
}

check_crates_channel() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "[crates] cargo is required" >&2
    return 1
  fi
  local cargo_home="$scratch_root/cargo-home"
  local cargo_root="$scratch_root/cargo-root"
  mkdir -p "$cargo_home" "$cargo_root"
  CARGO_HOME="$cargo_home" cargo install plasmite --version "$VERSION" --locked --force --root "$cargo_root" >/dev/null
  "$cargo_root/bin/plasmite" --version | grep -q "$VERSION"
}

check_homebrew_channel() {
  if ! command -v brew >/dev/null 2>&1; then
    echo "[homebrew] brew unavailable on this host; skipping" >&2
    return 3
  fi
  if ! command -v jq >/dev/null 2>&1; then
    echo "[homebrew] jq is required for version parsing" >&2
    return 1
  fi

  local stable
  stable="$(brew info sandover/tap/plasmite --json=v2 | jq -r '.formulae[0].versions.stable // empty')"
  if [[ "$stable" != "$VERSION" ]]; then
    echo "[homebrew] expected stable=$VERSION, got '${stable:-<empty>}'" >&2
    return 1
  fi
}

run_channel_with_retry() {
  local channel="$1"
  local fn_name="$2"
  local attempt=1
  local delay=10

  while true; do
    echo "[$channel] attempt $attempt"
    set +e
    "$fn_name"
    local status=$?
    set -e

    if [[ $status -eq 0 ]]; then
      echo "[$channel] OK"
      return 0
    fi
    if [[ $status -eq 3 ]]; then
      echo "[$channel] SKIP"
      return 3
    fi

    local now
    now="$(date +%s)"
    if [[ $now -ge $deadline_epoch ]]; then
      echo "[$channel] FAIL (retry budget exhausted)" >&2
      return 1
    fi

    local remaining="$(( deadline_epoch - now ))"
    local sleep_for="$delay"
    if [[ $sleep_for -gt $remaining ]]; then
      sleep_for="$remaining"
    fi
    if [[ $sleep_for -le 0 ]]; then
      echo "[$channel] FAIL (no time remaining)" >&2
      return 1
    fi

    echo "[$channel] retrying in ${sleep_for}s"
    sleep "$sleep_for"
    attempt="$((attempt + 1))"
    if [[ $delay -lt 60 ]]; then
      delay="$((delay * 2))"
    fi
  done
}

validate_channel_name() {
  case "$1" in
    npm|pypi|crates|homebrew)
      ;;
    *)
      echo "error: unknown channel '$1'" >&2
      exit 2
      ;;
  esac
}

IFS=',' read -r -a selected_channels <<< "$CHANNELS"
if [[ ${#selected_channels[@]} -eq 0 ]]; then
  echo "error: no channels selected" >&2
  exit 2
fi

summary_file="$scratch_root/summary.txt"
: > "$summary_file"
required_failures=0

for raw_channel in "${selected_channels[@]}"; do
  channel="$(echo "$raw_channel" | xargs)"
  if [[ -z "$channel" ]]; then
    continue
  fi
  validate_channel_name "$channel"

  fn="check_${channel}_channel"
  set +e
  run_channel_with_retry "$channel" "$fn"
  status=$?
  set -e

  if [[ $status -eq 0 ]]; then
    echo "$channel: PASS" >> "$summary_file"
    continue
  fi
  if [[ $status -eq 3 ]]; then
    echo "$channel: SKIP" >> "$summary_file"
    continue
  fi

  if is_required_channel "$channel"; then
    echo "$channel: FAIL (required)" >> "$summary_file"
    required_failures="$((required_failures + 1))"
  else
    echo "$channel: FAIL (advisory)" >> "$summary_file"
  fi
done

echo
echo "Post-release delivery smoke summary"
cat "$summary_file"

if [[ $required_failures -gt 0 ]]; then
  echo "error: $required_failures required channel(s) failed" >&2
  exit 1
fi

echo "All required channels passed."
