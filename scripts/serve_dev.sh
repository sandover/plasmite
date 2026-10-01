#!/usr/bin/env bash
# Purpose: Shared helpers for local Justfile dev-server workflows.
# Exports: `seed-demo`, `start-detached`, `run-with`, `stop`, and `status`.
# Invariants: Only the tracked process with this binary, directory, and bind is stopped.
# Invariants: Both listeners stay on loopback; HTTPS uses a free ephemeral port.
set -euo pipefail

canonical_bin() {
  printf '%s/%s\n' "$(cd -- "$(dirname -- "$1")" && pwd)" "$(basename -- "$1")"
}

seed_demo() {
  local bin="$1" pool_dir="$2" label="$3"
  mkdir -p "$pool_dir"
  if [[ -f "$pool_dir/demo.plasmite" ]]; then
    return 0
  fi
  "$bin" --dir "$pool_dir" pool create demo --size 1M >/dev/null
  "$bin" --dir "$pool_dir" feed demo --tag deploy '{"service":"api","version":"1.0"}' >/dev/null
  "$bin" --dir "$pool_dir" feed demo --tag metric '{"cpu":12.3,"rps":4200}' >/dev/null
  echo "${label}: seeded demo pool with 2 messages"
}

# A stale/reused PID must never authorize stopping another process.
owned_pid() {
  local pid="$1" bin="$2" pool_dir="$3" bind="$4"
  [[ "$pid" =~ ^[0-9]+$ ]] && [[ "$pid" -gt 1 ]] || return 1
  local actual
  actual="$(ps -ww -p "$pid" -o args= 2>/dev/null)" || return 1
  actual="${actual#"${actual%%[![:space:]]*}"}"
  [[ "$actual" == "$bin --dir $pool_dir serve --bind $bind --remote-bind 127.0.0.1:0" ]]
}

stop_pid() {
  local pid="$1" bin="$2" pool_dir="$3" bind="$4" label="$5"
  if ! kill -0 "$pid" 2>/dev/null; then
    return 0
  fi
  if ! owned_pid "$pid" "$bin" "$pool_dir" "$bind"; then
    echo "${label}: refusing to stop pid ${pid}; it does not match the tracked dev server" >&2
    return 1
  fi
  kill "$pid"
  local attempt
  for ((attempt=0; attempt<30; attempt++)); do
    if ! owned_pid "$pid" "$bin" "$pool_dir" "$bind"; then
      echo "${label}: stopped dev server (pid ${pid})"
      return 0
    fi
    sleep 0.1
  done
  echo "${label}: dev server has not stopped; keeping its tracked pid" >&2
  return 1
}

stop_tracked() {
  local bin="$1" pool_dir="$2" bind="$3" pid_file="$4" label="$5"
  [[ -f "$pid_file" ]] || return 0
  local pid
  pid="$(cat "$pid_file")"
  if [[ ! "$pid" =~ ^[0-9]+$ ]] || [[ "$pid" -le 1 ]]; then
    echo "${label}: invalid dev pid file: ${pid_file}" >&2
    return 1
  fi
  if kill -0 "$pid" 2>/dev/null; then
    bin="$(canonical_bin "$bin")"
    pool_dir="$(cd -- "$pool_dir" && pwd)"
    stop_pid "$pid" "$bin" "$pool_dir" "$bind" "$label"
  fi
  rm -f "$pid_file"
}

status_tracked() {
  local bin="$1" pool_dir="$2" bind="$3" pid_file="$4" log_file="$5"
  if [[ -f "$pid_file" && -d "$pool_dir" ]]; then
    local pid
    pid="$(cat "$pid_file")"
    bin="$(canonical_bin "$bin")"
    pool_dir="$(cd -- "$pool_dir" && pwd)"
    if owned_pid "$pid" "$bin" "$pool_dir" "$bind"; then
      echo "serve-status: running (pid ${pid})"
      echo "serve-status: http://${bind}/ui"
      echo "serve-status: log at ${log_file}"
      return 0
    fi
  fi
  echo "serve-status: not running"
}

start_detached() {
  local bin="$1" pool_dir="$2" bind="$3" log_file="$4" pid_file="$5" label="$6"
  bin="$(canonical_bin "$bin")"
  pool_dir="$(cd -- "$pool_dir" && pwd)"
  local -a args=(--dir "$pool_dir" serve --bind "$bind" --remote-bind 127.0.0.1:0)
  nohup "$bin" "${args[@]}" >"$log_file" 2>&1 &
  local pid=$!
  echo "$pid" >"$pid_file"
  sleep 0.5
  if ! owned_pid "$pid" "$bin" "$pool_dir" "$bind"; then
    rm -f "$pid_file"
    echo "${label}: server exited immediately; check ${log_file}" >&2
    cat "$log_file" >&2
    return 1
  fi
}

run_with() {
  local bin="$1" pool_dir="$2" bind="$3" log_file="$4" label="$5" cmd="$6"
  bin="$(canonical_bin "$bin")"
  pool_dir="$(cd -- "$pool_dir" && pwd)"
  "$bin" --dir "$pool_dir" serve --bind "$bind" --remote-bind 127.0.0.1:0 >"$log_file" 2>&1 &
  local pid=$!
  # EXIT runs after this function's local variables disappear.
  serve_dev_cleanup_args=("$pid" "$bin" "$pool_dir" "$bind" "$label")
  trap 'stop_pid "${serve_dev_cleanup_args[@]}" >/dev/null 2>&1 || true' EXIT
  sleep 0.5
  if ! owned_pid "$pid" "$bin" "$pool_dir" "$bind"; then
    echo "${label}: server exited immediately; check ${log_file}" >&2
    cat "$log_file" >&2
    return 1
  fi
  local status=0
  bash -lc "$cmd" || status=$?
  stop_pid "$pid" "$bin" "$pool_dir" "$bind" "$label"
  wait "$pid" 2>/dev/null || true
  trap - EXIT
  return "$status"
}

if [[ "$#" -lt 1 ]]; then
  echo "usage: serve_dev.sh <seed-demo|start-detached|run-with|stop|status> ..." >&2
  exit 2
fi
subcommand="$1"
shift
case "$subcommand" in
  seed-demo) seed_demo "$@" ;;
  start-detached) start_detached "$@" ;;
  run-with) run_with "$@" ;;
  stop) stop_tracked "$@" ;;
  status) status_tracked "$@" ;;
  *) echo "unknown subcommand: ${subcommand}" >&2; exit 2 ;;
esac
