#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PLASMITE_BIN="${PLASMITE_BIN:-${ROOT_DIR}/target/debug/plasmite}"
WORK_DIR="${ROOT_DIR}/tmp/cookbook-smoke"
POOL_DIR="${WORK_DIR}/pools"
SERVE_PID=""

cleanup() {
  if [[ -n "${SERVE_PID}" ]]; then
    kill "${SERVE_PID}" 2>/dev/null || true
  fi
  rm -rf "${WORK_DIR}"
}

trap cleanup EXIT

fail() {
  local message="$1"
  echo "cookbook smoke failure: ${message}" >&2
  cleanup
  exit 1
}

assert_contains() {
  local file="$1"
  local expected="$2"
  local label="$3"
  if ! grep -qF -- "${expected}" "${file}"; then
    fail "${label}: expected '${expected}' in ${file}"
  fi
}

assert_line_count_at_least() {
  local file="$1"
  local min_lines="$2"
  local label="$3"
  local count
  count=$(wc -l <"${file}" | tr -d ' ')
  if [[ "${count}" -lt "${min_lines}" ]]; then
    fail "${label}: expected at least ${min_lines} line(s), got ${count}"
  fi
}

assert_file_non_empty() {
  local file="$1"
  local label="$2"
  if [[ ! -s "${file}" ]]; then
    fail "${label}: expected non-empty file ${file}"
  fi
}

assert_jq_true() {
  local file="$1"
  local filter="$2"
  local label="$3"
  if ! jq -e "${filter}" "${file}" >/dev/null; then
    fail "${label}: expected jq filter '${filter}' to be true for ${file}"
  fi
}

run_binding_fixtures() {
  local fixtures_dir="${WORK_DIR}/fixtures"
  local python_out="${WORK_DIR}/python_fixture.json"
  local python_home_out="${WORK_DIR}/python_home_fixture.json"
  local node_out="${WORK_DIR}/node_fixture.json"
  local node_home_out="${WORK_DIR}/node_home_fixture.json"
  local go_out="${WORK_DIR}/go_fixture.json"
  local fresh_home="${fixtures_dir}/fresh-home"

  mkdir -p "${fixtures_dir}"
  rm -rf "${fresh_home}"
  mkdir -p "${fresh_home}"

  PYTHONPATH="${ROOT_DIR}/bindings/python" \
    PLASMITE_LIB_DIR="${ROOT_DIR}/target/debug" \
    python3 "${ROOT_DIR}/bindings/python/cmd/cookbook_smoke_fixture.py" "${fixtures_dir}/python-pools" >"${python_out}"
  assert_jq_true "${python_out}" '.data.task == "resize"' "Python cookbook fixture"
  assert_jq_true "${python_out}" '.tags == ["cookbook"]' "Python cookbook fixture"

  HOME="${fresh_home}" \
    PYTHONPATH="${ROOT_DIR}/bindings/python" \
    PLASMITE_LIB_DIR="${ROOT_DIR}/target/debug" \
    python3 - >"${python_home_out}" <<'PY'
import json
from pathlib import Path

from plasmite import Client, Durability

home = Path.home()
pool_dir = home / ".plasmite" / "pools"
pool_path = pool_dir / "cookbook-home.plasmite"

with Client() as client:
    with client.pool("cookbook-home", 1024 * 1024) as pool:
        msg = pool.append({"task": "resize", "id": 1}, ["cookbook"], Durability.FAST)
        pool.get(msg.seq)
        print(json.dumps({"seq": msg.seq, "tags": msg.tags, "data": msg.data}))

if not pool_path.exists():
    raise RuntimeError(f"expected pool to exist: {pool_path}")
PY
  assert_jq_true "${python_home_out}" '.data.task == "resize"' "Python fresh HOME fixture"
  assert_jq_true "${python_home_out}" '.tags == ["cookbook"]' "Python fresh HOME fixture"

  if [[ ! -d "${ROOT_DIR}/bindings/node/node_modules" ]]; then
    (cd "${ROOT_DIR}/bindings/node" && npm install)
  fi
  (cd "${ROOT_DIR}/bindings/node" && npm run build >/dev/null && npm run prepare-native >/dev/null)
  PLASMITE_LIB_DIR="${ROOT_DIR}/target/debug" \
    node "${ROOT_DIR}/bindings/node/cookbook_smoke_fixture.js" "${fixtures_dir}/node-pools" >"${node_out}"
  assert_jq_true "${node_out}" '.data.task == "resize"' "Node cookbook fixture"
  assert_jq_true "${node_out}" '.tags == ["cookbook"]' "Node cookbook fixture"

  (
    cd "${ROOT_DIR}/bindings/node"
    HOME="${fresh_home}" \
      PLASMITE_LIB_DIR="${ROOT_DIR}/target/debug" \
      node - >"${node_home_out}" <<'NODE'
const fs = require("node:fs");
const path = require("node:path");
const os = require("node:os");

const { Client } = require("./index.js");

const home = os.homedir();
const poolPath = path.join(home, ".plasmite", "pools", "cookbook-home.plasmite");

const client = new Client();
let pool = null;
try {
  pool = client.pool("cookbook-home", 1024 * 1024);
  const msg = pool.append({ task: "resize", id: 1 }, ["cookbook"]);
  pool.get(msg.seq);
  process.stdout.write(`${JSON.stringify({ seq: Number(msg.seq), tags: msg.tags, data: msg.data })}\n`);
} finally {
  if (pool) {
    pool.close();
  }
  client.close();
}

if (!fs.existsSync(poolPath)) {
  throw new Error(`expected pool to exist: ${poolPath}`);
}
NODE
  )
  assert_jq_true "${node_home_out}" '.data.task == "resize"' "Node fresh HOME fixture"
  assert_jq_true "${node_home_out}" '.tags == ["cookbook"]' "Node fresh HOME fixture"

  mkdir -p "${WORK_DIR}/go-cache" "${WORK_DIR}/go-tmp"
  (
    cd "${ROOT_DIR}/bindings/go" && \
      GOCACHE="${WORK_DIR}/go-cache" \
      GOTMPDIR="${WORK_DIR}/go-tmp" \
      PLASMITE_LIB_DIR="${ROOT_DIR}/target/debug" \
      LD_LIBRARY_PATH="${ROOT_DIR}/target/debug:${LD_LIBRARY_PATH:-}" \
      DYLD_FALLBACK_LIBRARY_PATH="${ROOT_DIR}/target/debug:${DYLD_FALLBACK_LIBRARY_PATH:-}" \
      PKG_CONFIG="/usr/bin/true" \
      CGO_CFLAGS="-I${ROOT_DIR}/include" \
      CGO_LDFLAGS="-L${ROOT_DIR}/target/debug" \
      go run ./cmd/cookbook-smoke-fixture "${fixtures_dir}/go-pools"
  ) >"${go_out}"
  assert_jq_true "${go_out}" '.data.task == "resize"' "Go cookbook fixture"
  assert_jq_true "${go_out}" '.tags == ["cookbook"]' "Go cookbook fixture"
}

pick_port() {
  python3 - <<'PY'
import socket

for port in range(9700, 9800):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        try:
            sock.bind(("127.0.0.1", port))
            print(port)
            raise SystemExit
        except OSError:
            pass

print("")
PY
}

if [[ ! -x "${PLASMITE_BIN}" ]]; then
  echo "building plasmite..."
  (cd "${ROOT_DIR}" && cargo build -q --bin plasmite)
fi

if ! compgen -G "${ROOT_DIR}/target/debug/libplasmite.{dylib,so}" >/dev/null; then
  echo "building libplasmite (cdylib)..."
  (cd "${ROOT_DIR}" && cargo build -q --lib)
fi

rm -rf "${WORK_DIR}"
mkdir -p "${POOL_DIR}"

# Produce & Consume
produce_out="${WORK_DIR}/produce_consume.jsonl"
"${PLASMITE_BIN}" --dir "${POOL_DIR}" feed work --create '{"task":"resize","id":1}' >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" follow work --tail 1 --one --jsonl >"${produce_out}"
assert_contains "${produce_out}" '"task":"resize"' "Produce & Consume"

# CI Gate
ci_out="${WORK_DIR}/ci_gate_follow.jsonl"
"${PLASMITE_BIN}" --dir "${POOL_DIR}" pool create ci >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" feed ci '{"status":"green","commit":"a1b2c3","suite":"unit"}' >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" follow ci --where '.data.status == "green"' --tail 1 --one --jsonl >"${ci_out}"
assert_contains "${ci_out}" '"status":"green"' "CI Gate"

# Live Build Progress
build_out="${WORK_DIR}/live_build_follow.jsonl"
"${PLASMITE_BIN}" --dir "${POOL_DIR}" pool create build >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" feed build '{"step":"compile","pct":0}' >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" feed build '{"step":"compile","pct":100}' >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" feed build '{"step":"test","pct":0}' >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" feed build '{"step":"test","pct":100}' >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" feed build '{"step":"finished","ok":true}' --tag done >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" follow build --tag done --tail 1 --one --jsonl >"${build_out}"
assert_contains "${build_out}" '"ok":true' "Live Build Progress"

# Multi-Writer Event Bus
multi_out="${WORK_DIR}/multi_writer_follow.jsonl"
"${PLASMITE_BIN}" --dir "${POOL_DIR}" pool create events >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" feed events '{"service":"api","sha":"f4e5d6"}' --tag deploy >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" feed events '{"service":"api","msg":"latency spike"}' --tag alert >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" feed events '{"service":"web","rps":1420}' --tag metric >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" follow events --tag alert --tail 1 --one --jsonl >"${multi_out}"
assert_contains "${multi_out}" '"msg":"latency spike"' "Multi-Writer Event Bus"

# Replay & Debug
replay_out="${WORK_DIR}/replay.jsonl"
replay_filter_out="${WORK_DIR}/replay_debug.jsonl"
"${PLASMITE_BIN}" --dir "${POOL_DIR}" pool create incidents >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" feed incidents '{"level":"info","msg":"booting"}' >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" feed incidents '{"level":"error","msg":"latency spike"}' >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" follow incidents --replay 0 --since 1m --jsonl >"${replay_out}"
assert_line_count_at_least "${replay_out}" 2 "Replay & Debug"
"${PLASMITE_BIN}" --dir "${POOL_DIR}" follow incidents --where '.data.level == "error"' --tail 1 --one --jsonl >"${replay_filter_out}"
assert_contains "${replay_filter_out}" '"level":"error"' "Replay & Debug"
assert_file_non_empty "${replay_filter_out}" "Replay & Debug"

# Remote Pool Access
remote_port="$(pick_port)"
if [[ -z "${remote_port}" ]]; then
  fail "could not allocate a free localhost port"
fi
remote_url="http://127.0.0.1:${remote_port}"
remote_follow_out="${WORK_DIR}/remote_follow.jsonl"

"${PLASMITE_BIN}" --dir "${POOL_DIR}" pool create remote-events >/dev/null
"${PLASMITE_BIN}" --dir "${POOL_DIR}" serve --bind "127.0.0.1:${remote_port}" >"${WORK_DIR}/remote-serve.log" 2>&1 &
SERVE_PID=$!

for _ in $(seq 1 60); do
  if curl -fsS "${remote_url}/healthz" >/dev/null; then
    remote_ready=true
    break
  fi
  sleep 0.05
done

if [[ "${remote_ready:-false}" != true ]]; then
  fail "remote server was not ready after startup wait"
fi

if ! kill -0 "${SERVE_PID}" 2>/dev/null; then
  fail "remote server exited before readiness checks completed"
fi

"${PLASMITE_BIN}" feed "${remote_url}/remote-events" '{"sensor":"temp","value":23.5}' >/dev/null
"${PLASMITE_BIN}" follow "${remote_url}/remote-events" --tail 1 --one --jsonl >"${remote_follow_out}"
assert_contains "${remote_follow_out}" '"sensor":"temp"' "Remote Pool Access"

SERVE_PID=""

run_binding_fixtures

echo "cookbook smoke checks passed"
