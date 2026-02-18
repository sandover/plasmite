#!/usr/bin/env bash
# Purpose: Shared helpers for local Justfile dev-server workflows.
# Exports: Script entrypoint with `seed-demo`, `start-detached`, `run-with`.
# Role: Remove duplicated setup/seed/start logic across serve-dev recipes.
# Invariants: Demo seeding is idempotent and only runs when demo pool is missing.
# Invariants: Start commands fail closed if the server exits immediately.
# Notes: Intended to be called from Just recipes only.

set -euo pipefail

seed_demo() {
  local bin="$1"
  local pool_dir="$2"
  local mode="$3"
  local label="$4"

  mkdir -p "$pool_dir"
  if [[ -f "$pool_dir/demo.plasmite" ]]; then
    return 0
  fi

  "$bin" --dir "$pool_dir" pool create demo --size 1M >/dev/null
  "$bin" --dir "$pool_dir" feed demo --tag deploy '{"service":"api","version":"1.0"}' >/dev/null
  if [[ "$mode" == "full" ]]; then
    "$bin" --dir "$pool_dir" feed demo --tag metric '{"cpu":12.3,"rps":4200}' >/dev/null
    echo "${label}: seeded demo pool with 2 messages"
  else
    echo "${label}: seeded demo pool with 1 message"
  fi
}

start_detached() {
  local bin="$1"
  local pool_dir="$2"
  local bind="$3"
  local log_file="$4"
  local pid_file="$5"
  local token="$6"
  local label="$7"

  local -a args=(--dir "$pool_dir" serve --bind "$bind")
  if [[ -n "$token" ]]; then
    args+=(--token "$token")
  fi

  nohup "$bin" "${args[@]}" >"$log_file" 2>&1 & echo $! >"$pid_file"
  sleep 0.5
  if ! kill -0 "$(cat "$pid_file")" 2>/dev/null; then
    echo "${label}: ERROR — server exited immediately. Check ${log_file}"
    cat "$log_file"
    exit 1
  fi
}

run_with() {
  local bin="$1"
  local pool_dir="$2"
  local bind="$3"
  local log_file="$4"
  local label="$5"
  local cmd="$6"

  "$bin" --dir "$pool_dir" serve --bind "$bind" >"$log_file" 2>&1 &
  local pid=$!
  trap 'kill "$pid" 2>/dev/null || true' EXIT
  sleep 0.5
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "${label}: ERROR — server exited immediately. Check ${log_file}"
    cat "$log_file"
    exit 1
  fi
  bash -lc "$cmd"
}

if [[ "$#" -lt 1 ]]; then
  echo "usage: serve_dev.sh <seed-demo|start-detached|run-with> ..." >&2
  exit 2
fi

subcommand="$1"
shift

case "$subcommand" in
  seed-demo)
    seed_demo "$@"
    ;;
  start-detached)
    start_detached "$@"
    ;;
  run-with)
    run_with "$@"
    ;;
  *)
    echo "unknown subcommand: ${subcommand}" >&2
    exit 2
    ;;
esac
