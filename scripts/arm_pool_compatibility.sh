#!/usr/bin/env bash
# Verify both directions of the pool format across 32-bit ARM and a 64-bit host.
# Usage: arm_pool_compatibility.sh <native-64-bit-plasmite> <armv7-plasmite>
set -euo pipefail
if [[ $# != 2 ]]; then
  echo "usage: $0 <native-64-bit-plasmite> <armv7-plasmite>" >&2
  exit 2
fi
native_cli="$1"
arm_cli="$2"
sysroot="${ARMV7_SYSROOT:-/usr/arm-linux-gnueabihf}"
workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT
run_cli() {
  local width="$1"
  shift
  if [[ "$width" == 32 ]]; then
    qemu-arm -L "$sysroot" "$arm_cli" "$@"
  else
    "$native_cli" "$@"
  fi
}
check_direction() {
  local writer="$1" reader="$2"
  local directory="$workdir/$writer-to-$reader"
  run_cli "$writer" --dir "$directory" pool create --size 64K wrapped >/dev/null
  for ((i=1; i<=450; i++)); do
    printf '{"message":"tick %d","level":"info"}\n' "$i"
  done | run_cli "$writer" --dir "$directory" feed wrapped >/dev/null
  local original_info read_info oldest newest message
  original_info="$(run_cli "$writer" --dir "$directory" pool info wrapped --json)"
  read_info="$(run_cli "$reader" --dir "$directory" pool info wrapped --json)"
  [[ "$(jq -c '.bounds' <<<"$original_info")" == "$(jq -c '.bounds' <<<"$read_info")" ]]
  oldest="$(jq -r '.bounds.oldest' <<<"$read_info")"
  newest="$(jq -r '.bounds.newest' <<<"$read_info")"
  [[ "$oldest" -gt 1 && "$newest" == 450 ]]
  for ((i=oldest; i<=newest; i++)); do
    message="$(run_cli "$reader" --dir "$directory" fetch wrapped "$i" --json)"
    jq -e --argjson seq "$i" '.seq == $seq and .data.message == ("tick " + ($seq|tostring)) and .data.level == "info"' <<<"$message" >/dev/null
  done
  printf '{"message":"after handoff"}\n' | run_cli "$reader" --dir "$directory" feed wrapped >/dev/null
  message="$(run_cli "$writer" --dir "$directory" fetch wrapped 451 --json)"
  jq -e '.seq == 451 and .data.message == "after handoff"' <<<"$message" >/dev/null
  run_cli "$writer" --dir "$directory" doctor wrapped --json >/dev/null
  run_cli "$reader" --dir "$directory" doctor wrapped --json >/dev/null
  echo "pool compatibility $writer -> $reader: passed"
}
check_direction 64 32
check_direction 32 64
