#!/usr/bin/env bash
# Purpose: Compare benchmark medians for current release candidate vs a base tag on the same host.
# Key outputs: JSON + Markdown summaries under .scratch/release with per-scenario regressions.
# Role: Release-blocking performance gate for maintainers before tagging/publishing.
# Invariants: Uses one host/power mode for both sides; runs each side multiple times.
# Invariants: Fails closed on missing comparisons or >= threshold regressions in core benches.
# Notes: CI canary benchmarks are advisory only; this script is the release decision source.

set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  compare_local_benchmarks.sh \
    --base-tag <vX.Y.Z> \
    [--runs 3] \
    [--messages 5000] \
    [--threshold-percent 15] \
    [--noise-floor 0.0001]
USAGE
}

BASE_TAG=""
RUNS=3
MESSAGES=5000
THRESHOLD_PERCENT=15
NOISE_FLOOR=0.0001

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base-tag)
      BASE_TAG="${2:-}"
      shift 2
      ;;
    --runs)
      RUNS="${2:-}"
      shift 2
      ;;
    --messages)
      MESSAGES="${2:-}"
      shift 2
      ;;
    --threshold-percent)
      THRESHOLD_PERCENT="${2:-}"
      shift 2
      ;;
    --noise-floor)
      NOISE_FLOOR="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if [[ -z "$BASE_TAG" ]]; then
  echo "error: --base-tag is required" >&2
  usage
  exit 2
fi
if [[ ! "$BASE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: base tag must match vX.Y.Z (got '$BASE_TAG')" >&2
  exit 2
fi
if ! [[ "$RUNS" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: --runs must be a positive integer" >&2
  exit 2
fi
if ! [[ "$MESSAGES" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: --messages must be a positive integer" >&2
  exit 2
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
OUT_ROOT="$ROOT/.scratch/release/bench-compare-${BASE_TAG}-$(date +%Y%m%d-%H%M%S)"
BASE_WORKTREE="$OUT_ROOT/base-worktree"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
mkdir -p "$OUT_ROOT/base" "$OUT_ROOT/current"

cleanup() {
  git -C "$ROOT" worktree remove --force "$BASE_WORKTREE" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for cmd in git cargo jq; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: required tool missing: $cmd" >&2
    exit 2
  fi
done

if ! git -C "$ROOT" rev-parse --verify --quiet "refs/tags/$BASE_TAG" >/dev/null; then
  echo "error: tag '$BASE_TAG' not found locally." >&2
  echo "hint: run 'git fetch --tags' and retry." >&2
  exit 2
fi

run_suite() {
  local repo_dir="$1"
  local label="$2"
  local out_dir="$3"
  local run

  (
    cd "$repo_dir"
    CARGO_TARGET_DIR="$TARGET_DIR" cargo build --release --example plasmite-bench
    for run in $(seq 1 "$RUNS"); do
      "$TARGET_DIR/release/examples/plasmite-bench" \
        --messages "$MESSAGES" \
        --format json > "$out_dir/${label}-run-${run}.json"
    done
  )
}

echo "info: benchmarking current candidate ($RUNS runs, messages=$MESSAGES)"
run_suite "$ROOT" "current" "$OUT_ROOT/current"

echo "info: benchmarking base tag $BASE_TAG ($RUNS runs, messages=$MESSAGES)"
git -C "$ROOT" worktree add --detach "$BASE_WORKTREE" "$BASE_TAG" >/dev/null
run_suite "$BASE_WORKTREE" "base" "$OUT_ROOT/base"

aggregate_medians() {
  local out_json="$1"
  shift

  if [[ "$#" -eq 0 ]]; then
    echo "error: no benchmark JSON files were provided for aggregation." >&2
    return 1
  fi

  jq -s '
    def median:
      sort as $vals
      | if ($vals | length) == 0 then null
        elif ($vals | length) % 2 == 1 then $vals[(($vals | length) / 2 | floor)]
        else (($vals[($vals | length)/2 - 1] + $vals[($vals | length)/2]) / 2)
        end;

    map(
      .results[]
      | select(.bench == "append" or .bench == "multi_writer" or .bench == "get_scan")
      | {
          bench,
          key: (
            .bench
            + "|dur=" + (.durability // "n/a")
            + "|writers=" + ((.writers // 0) | tostring)
            + "|payload=" + ((.payload_bytes // 0) | tostring)
            + "|pool=" + ((.pool_size // 0) | tostring)
            + "|notes=" + (.notes // "n/a")
          ),
          ms: (.ms_per_msg // 0)
        }
    )
    | group_by(.key)
    | map({
        bench: .[0].bench,
        key: .[0].key,
        median_ms_per_msg: (map(.ms) | median),
        samples: length
      })
  ' "$@" > "$out_json"
}

BASE_MEDIANS_JSON="$OUT_ROOT/base-medians.json"
CURRENT_MEDIANS_JSON="$OUT_ROOT/current-medians.json"
SUMMARY_JSON="$OUT_ROOT/summary.json"
SUMMARY_MD="$OUT_ROOT/summary.md"

base_inputs=( "$OUT_ROOT"/base/base-run-*.json )
current_inputs=( "$OUT_ROOT"/current/current-run-*.json )

if [[ "${base_inputs[0]:-}" == "$OUT_ROOT/base/base-run-*.json" ]]; then
  echo "error: base benchmark runs not found under $OUT_ROOT/base" >&2
  exit 1
fi
if [[ "${current_inputs[0]:-}" == "$OUT_ROOT/current/current-run-*.json" ]]; then
  echo "error: current benchmark runs not found under $OUT_ROOT/current" >&2
  exit 1
fi

aggregate_medians "$BASE_MEDIANS_JSON" "${base_inputs[@]}"
aggregate_medians "$CURRENT_MEDIANS_JSON" "${current_inputs[@]}"

ENV_BASE_TAG="$BASE_TAG" \
ENV_RUNS="$RUNS" \
ENV_MESSAGES="$MESSAGES" \
jq -n \
  --argjson threshold "$THRESHOLD_PERCENT" \
  --argjson noise "$NOISE_FLOOR" \
  --slurpfile base "$BASE_MEDIANS_JSON" \
  --slurpfile cur "$CURRENT_MEDIANS_JSON" '
    ($base[0] | map({(.key): .}) | add) as $base_map
    | [
        $cur[0][]
        | . as $cur_row
        | ($base_map[$cur_row.key] // null) as $base_row
        | select($base_row != null)
        | ($base_row.median_ms_per_msg) as $base_ms
        | ($cur_row.median_ms_per_msg) as $cur_ms
        | {
            bench: $cur_row.bench,
            scenario_key: $cur_row.key,
            base_median_ms_per_msg: $base_ms,
            current_median_ms_per_msg: $cur_ms,
            regression_percent: (
              if $base_ms == 0 then null
              else (($cur_ms - $base_ms) / $base_ms * 100)
              end
            )
          }
        | .status = (
            if .regression_percent == null then "no_baseline"
            elif (.base_median_ms_per_msg < $noise and .current_median_ms_per_msg < $noise) then "noise_floor"
            elif .regression_percent >= $threshold then "regressed"
            else "ok"
            end
          )
      ] as $rows
    | {
        base_tag: env.ENV_BASE_TAG,
        runs_per_side: (env.ENV_RUNS | tonumber),
        messages: (env.ENV_MESSAGES | tonumber),
        threshold_percent: $threshold,
        noise_floor_ms_per_msg: $noise,
        comparable_scenarios: ($rows | length),
        regressions: ($rows | map(select(.status == "regressed"))),
        comparisons: $rows
      }
  ' > "$SUMMARY_JSON"

comparable_count="$(jq '.comparable_scenarios' "$SUMMARY_JSON")"
if [[ "$comparable_count" -eq 0 ]]; then
  echo "error: no comparable benchmark scenarios produced." >&2
  echo "hint: check benchmark output and ensure base/current runs used compatible scenario sets." >&2
  exit 1
fi

{
  echo "# Local Benchmark Comparison"
  echo
  echo "- Base tag: $BASE_TAG"
  echo "- Runs per side: $RUNS"
  echo "- Messages per run: $MESSAGES"
  echo "- Regression threshold: ${THRESHOLD_PERCENT}%"
  echo "- Noise floor: ${NOISE_FLOOR} ms/msg"
  echo
  echo "| Bench | Scenario | Base median ms/msg | Current median ms/msg | Regression % | Status |"
  echo "| --- | --- | ---: | ---: | ---: | --- |"
  jq -r '
    .comparisons[]
    | [
        .bench,
        .scenario_key,
        (.base_median_ms_per_msg | tostring),
        (.current_median_ms_per_msg | tostring),
        (.regression_percent | if . == null then "n/a" else tostring end),
        .status
      ]
    | @tsv
  ' "$SUMMARY_JSON" | while IFS=$'\t' read -r bench scenario base cur pct status; do
    printf '| %s | `%s` | %s | %s | %s | %s |\n' "$bench" "$scenario" "$base" "$cur" "$pct" "$status"
  done
} > "$SUMMARY_MD"

regression_count="$(jq '.regressions | length' "$SUMMARY_JSON")"
echo "info: benchmark comparison summary: $SUMMARY_JSON"
echo "info: benchmark comparison report:  $SUMMARY_MD"

if [[ "$regression_count" -gt 0 ]]; then
  echo "error: detected $regression_count regressed core benchmark scenario(s)." >&2
  echo "hint: inspect summary JSON/Markdown and file a release blocker if regression is unexplained." >&2
  exit 1
fi

echo "ok: local benchmark comparison passed"
