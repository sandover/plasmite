#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "${ROOT_DIR}/scripts/normalize_sdk_layout.sh"
PLASMITE_BIN="${PLASMITE_BIN:-${ROOT_DIR}/target/debug/plasmite}"
WORK_DIR="${ROOT_DIR}/tmp/cookbook-smoke"
POOL_DIR="${WORK_DIR}/pools"
SERVE_PID=""

cleanup() {
  if [[ -n "${SERVE_PID}" ]]; then
    kill "${SERVE_PID}" 2>/dev/null || true
    wait "${SERVE_PID}" 2>/dev/null || true
    SERVE_PID=""
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
    with client.open_pool("cookbook-home", create=True) as pool:
        sent = pool.feed({"task": "resize", "id": 1}, ["cookbook"], Durability.FAST)
        with pool.fetch(since=sent.seq) as reader:
            msg = next(reader)
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

(async () => {
  const client = new Client();
  let pool;
  try {
    pool = client.openPool("cookbook-home", { create: true });
    const sent = await pool.feed({ task: "resize", id: 1 }, { tags: ["cookbook"] });
    const reader = await pool.fetch({ since: sent.seq });
    const { value: msg } = await reader.next();
    await reader.return();
    process.stdout.write(`${JSON.stringify({ seq: Number(msg.seq), tags: msg.tags, data: msg.data })}\n`);
  } finally {
    pool?.close();
    client.close();
  }
  if (!fs.existsSync(poolPath)) throw new Error(`expected pool to exist: ${poolPath}`);
})().catch(error => { console.error(error); process.exitCode = 1; });
NODE
  )
  assert_jq_true "${node_home_out}" '.data.task == "resize"' "Node fresh HOME fixture"
  assert_jq_true "${node_home_out}" '.tags == ["cookbook"]' "Node fresh HOME fixture"

  local go_cache="${GOCACHE:-${WORK_DIR}/go-cache}"
  local sdk_dir
  sdk_dir="$(plasmite_normalize_sdk_dir "${ROOT_DIR}/target/debug" "${WORK_DIR}/sdk" "cookbook SDK")"
  mkdir -p "${go_cache}" "${WORK_DIR}/go-tmp"
  (
    cd "${ROOT_DIR}/bindings/go" && \
      GOCACHE="${go_cache}" \
      GOTMPDIR="${WORK_DIR}/go-tmp" \
      PLASMITE_LIB_DIR="${sdk_dir}/lib" \
      LD_LIBRARY_PATH="${sdk_dir}/lib:${LD_LIBRARY_PATH:-}" \
      DYLD_LIBRARY_PATH="${sdk_dir}/lib:${DYLD_LIBRARY_PATH:-}" \
      PKG_CONFIG="/usr/bin/true" \
      CGO_CFLAGS="-I${ROOT_DIR}/include" \
      CGO_LDFLAGS="-L${sdk_dir}/lib" \
      go run ./cmd/cookbook-smoke-fixture "${fixtures_dir}/go-pools"
  ) >"${go_out}"
  assert_jq_true "${go_out}" '.data.task == "resize"' "Go cookbook fixture"
  assert_jq_true "${go_out}" '.tags == ["cookbook"]' "Go cookbook fixture"
}

# Print a free localhost port for TCP or UDP (the first argument).
pick_port() {
  python3 - "$1" <<'PY'
import socket
import sys

kind = socket.SOCK_DGRAM if sys.argv[1] == "udp" else socket.SOCK_STREAM
for port in range(9700, 9800):
    with socket.socket(socket.AF_INET, kind) as sock:
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
bash "${ROOT_DIR}/scripts/stage_versioned_library.sh" "${ROOT_DIR}/target/debug"

rm -rf "${WORK_DIR}"
mkdir -p "${POOL_DIR}"

# Each block below runs the matching docs/cookbook.md recipe against a scratch pool directory.
plasmite() { "${PLASMITE_BIN}" --dir "${POOL_DIR}" "$@"; }
cd "${WORK_DIR}"

# Log events and watch them
plasmite feed builds --create '{"step": "compile", "status": "ok"}' >/dev/null
plasmite feed builds '{"step": "test", "status": "failed"}' >/dev/null
plasmite follow builds --last 10 | head -2 >log_follow.jsonl
assert_line_count_at_least log_follow.jsonl 2 "Log events and watch them"
plasmite fetch builds --since 1h >log_fetch.jsonl
assert_contains log_fetch.jsonl '"status":"failed"' "Log events and watch them"

# Gate a deploy on CI: the test job finishes before the deploy job starts waiting.
CI_RUN_ID=smoke
plasmite feed "ci-$CI_RUN_ID" --create --tag ok '{"step": "test", "status": "ok"}' >/dev/null
plasmite follow "ci-$CI_RUN_ID" --create --since oldest --tag ok --one >/dev/null && echo deployed >ci_gate.txt
assert_contains ci_gate.txt deployed "Gate a deploy on CI"

# Capture command output
printf 'compiling\n\nwarning: unused variable' | plasmite feed captured --create --input-format lines >/dev/null
plasmite fetch captured --last 10 >captured.jsonl
assert_line_count_at_least captured.jsonl 3 "Capture command output"
assert_contains captured.jsonl '"line":"warning: unused variable"' "Capture command output"

# Let several programs write to one pool
plasmite feed events --create --tag deploy '{"service": "api", "sha": "f4e5d6c"}' >/dev/null
plasmite feed events --tag error '{"service": "api", "msg": "latency spike"}' >/dev/null
plasmite feed events --tag metric '{"service": "web", "rps": 1420}' >/dev/null
plasmite feed events --tag deploy --tag api '{"service": "api", "sha": "a1b2c3d"}' >/dev/null
plasmite follow events --since oldest --tag error --one >writers.jsonl
assert_contains writers.jsonl '"msg":"latency spike"' "Let several programs write to one pool"

# Read only the messages you need
plasmite fetch events --since 1h --tag deploy --tag api >tags.jsonl
assert_contains tags.jsonl '"sha":"a1b2c3d"' "Read only the messages you need"
if grep -qF 'f4e5d6c' tags.jsonl; then fail "Read only the messages you need: --tag must require every tag"; fi
plasmite fetch events --last 100 | jq -c 'select(.data.service == "api")' >jq_filter.jsonl
assert_line_count_at_least jq_filter.jsonl 3 "Read only the messages you need"

# Resume after a restart: the pool wraps before the first run, so follow reports a gap.
plasmite pool create sensors --size 64K >/dev/null
for i in $(seq 1 300); do echo "{\"temp_c\": 21.4, \"sample\": $i}"; done | plasmite feed sensors >/dev/null
resume_once() {
  next=$(cat sensors.next 2>/dev/null || echo 1)
  plasmite follow sensors --since "$next" 2>>resume.err | head -2 | while read -r msg; do
    jq -c .data <<<"$msg"
    jq '.seq + 1' <<<"$msg" > sensors.next
  done
}
resume_once >resume1.jsonl
assert_contains resume.err '"kind":"RetentionGap"' "Resume after a restart"
first_next="$(cat sensors.next)"
resume_once >resume2.jsonl
[[ "$(cat sensors.next)" -eq $((first_next + 2)) ]] || fail "Resume after a restart: checkpoint did not advance"

# Save history to a file and replay it
plasmite fetch events --since 1h > incident.jsonl
plasmite feed replay --create < incident.jsonl >/dev/null
plasmite fetch replay --tag error >replayed.jsonl
assert_contains replayed.jsonl '"msg":"latency spike"' "Save history to a file and replay it"

# Chat between two terminals
printf '{"from": "ana", "text": "hi"}\n' | plasmite duplex chat --create --last 20 | head -1 >chat.jsonl
assert_contains chat.jsonl '"text":"hi"' "Chat between two terminals"

# Share a pool with a teammate and collect sensor readings over UDP.
# The smoke test uses the server's credential-free loopback listener in place of an access key.
http_port="$(pick_port tcp)"
udp_port="$(pick_port udp)"
if [[ -z "${http_port}" || -z "${udp_port}" ]]; then
  fail "could not allocate free localhost ports"
fi
plasmite serve --bind "127.0.0.1:${http_port}" --remote-bind 127.0.0.1:0 \
  --udp="127.0.0.1:${udp_port}" --udp-follow >serve.log 2>&1 &
SERVE_PID=$!

server_url="http://127.0.0.1:${http_port}"
for _ in $(seq 1 60); do
  if curl -fsS "${server_url}/healthz" >/dev/null 2>&1; then
    server_ready=true
    break
  fi
  sleep 0.05
done
if [[ "${server_ready:-false}" != true ]] || ! kill -0 "${SERVE_PID}" 2>/dev/null; then
  fail "server was not ready after startup wait"
fi

plasmite follow "${server_url}/builds" --last 20 | head -1 >remote.jsonl
assert_contains remote.jsonl '"step":"compile"' "Share a pool with a teammate"

plasmite feed "udp://127.0.0.1:${udp_port}/sensors" --tag sensor '{"temp_c": 21.4}' 2>/dev/null
for _ in $(seq 1 60); do
  if plasmite fetch sensors --tag sensor | grep -qF '"temp_c":21.4'; then
    udp_stored=true
    break
  fi
  sleep 0.05
done
[[ "${udp_stored:-false}" == true ]] || fail "Collect sensor readings over UDP: reading was not stored"
plasmite follow "udp://127.0.0.1:${udp_port}/sensors" --last 1 --one >udp.jsonl
assert_contains udp.jsonl '"tags":["sensor"]' "Collect sensor readings over UDP"

kill "${SERVE_PID}"
wait "${SERVE_PID}" 2>/dev/null || true
SERVE_PID=""
cd "${ROOT_DIR}"

run_binding_fixtures

echo "cookbook smoke checks passed"
