#!/usr/bin/env bash
# Exercise HTTPS through each binding and reject a library with the wrong ABI major.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=$(mktemp -d)
server_pid=
cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
bin="$root/target/debug/plasmite"
lib="$root/target/debug"
export PLASMITE_LIB_DIR="$lib"
export PLASMITE_ACCESS_HOME="$work/access"
mkdir -p "$work/pools" "$PLASMITE_ACCESS_HOME"
export PLASMITE_SERVE_READY_FILE="$work/address"
"$bin" --dir "$work/pools" serve --bind 127.0.0.1:0 --remote-bind 127.0.0.1:0 \
  --shared-address https://localhost:9743 >"$work/server.log" 2>&1 &
server_pid=$!
for _ in {1..200}; do
  [[ -s "$work/address" ]] && break
  if ! kill -0 "$server_pid" 2>/dev/null; then
    cat "$work/server.log" >&2
    exit 1
  fi
  sleep .05
done
[[ -s "$work/address" ]] || { cat "$work/server.log" >&2; exit 1; }
address="$(cat "$work/address")"
origin="https://localhost:${address##*:}"
key=$("$bin" --dir "$work/pools" access invite binding-test --format json | jq -er .access_key)
printf '%s\n' "$key" | "$bin" access connect "$origin" >/dev/null
unset key
export PLASMITE_TEST_HTTPS_ORIGIN="$origin"

PYTHONPATH="$root/bindings/python" python3 - <<'PY'
import os
import plasmite

ref = os.environ["PLASMITE_TEST_HTTPS_ORIGIN"] + "/python"
with plasmite.create_pool(ref, 65536) as created, plasmite.open_pool(ref) as pool:
    with pool.follow(tags=["test"]) as live:
        assert live.next(0.01) is None
        first = created.feed({"from": "python"}, tags=["test"])
        assert live.next(3).seq == first.seq
    with pool.fetch(last=1) as reader:
        messages = list(reader)
        assert len(messages) == 1 and messages[0].data == {"from": "python"}
PY

node - "$root/bindings/node" <<'JS'
const assert = require("node:assert/strict");
const { openPool, createPool } = require(process.argv[2]);
(async () => {
  const ref = process.env.PLASMITE_TEST_HTTPS_ORIGIN + "/node";
  const created = createPool(ref, 65536);
  const pool = openPool(ref);
  try {
    const live = await pool.follow({ tags: ["test"] });
    try {
      assert.equal((await live.next(0.01)).value, undefined);
      const first = await created.feed({ from: "node" }, { tags: ["test"] });
      assert.equal((await live.next(3)).value.seq, first.seq);
    } finally { live.close(); }
    const reader = await pool.fetch({ last: 1 });
    const messages = [];
    for await (const message of reader) messages.push(message.data);
    assert.deepEqual(messages, [{ from: "node" }]);
  } finally {
    pool.close();
    created.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
JS

case "$(uname -s)" in
  Darwin) name=libplasmite.2.dylib; flags=(-dynamiclib -Wl,-install_name,@rpath/libplasmite.2.dylib) ;;
  Linux) name=libplasmite.so.2; flags=(-shared -fPIC -Wl,-soname,libplasmite.so.2) ;;
  *) echo "Run native binding contract smoke on macOS or Linux" >&2; exit 1 ;;
esac
export PLASMITE_WRONG_ABI="$work/wrong"
mkdir -p "$PLASMITE_WRONG_ABI"
{
  printf 'unsigned int plsm_abi_major(void) { return 1; }\n'
  nm -g "$lib/$name" | awk '$2 == "T" && $3 ~ /^_?plsm_/ {
    sub(/^_/, "", $3)
    if ($3 != "plsm_abi_major") print "void " $3 "(void) {}"
  }'
} >"$work/wrong.c"
"${CC:-cc}" "${flags[@]}" "$work/wrong.c" -o "$PLASMITE_WRONG_ABI/$name"

export GOCACHE="${GOCACHE:-$root/tmp/go-cache}"
export CGO_CFLAGS="-I$root/include"
export CGO_LDFLAGS="-L$lib"
export PKG_CONFIG=/usr/bin/true
export DYLD_LIBRARY_PATH="$lib"
export LD_LIBRARY_PATH="$lib"
(
  cd "$root/bindings/go"
  go test ./local -run '^TestHTTPSPoolReference$' -count=1
  go test -c ./local -o "$work/go-test"
)
(
  export DYLD_LIBRARY_PATH="$PLASMITE_WRONG_ABI" LD_LIBRARY_PATH="$PLASMITE_WRONG_ABI"
  "$work/go-test" -test.run '^TestRejectsWrongABIMajor$'
)

DYLD_LIBRARY_PATH="$PLASMITE_WRONG_ABI" LD_LIBRARY_PATH="$PLASMITE_WRONG_ABI" \
  PLASMITE_LIB_DIR="$PLASMITE_WRONG_ABI" PYTHONPATH="$root/bindings/python" python3 - <<'PY'
try:
    import plasmite
except ImportError as error:
    assert "requires ABI major 2; found 1" in str(error), error
else:
    raise AssertionError("Python loaded ABI major 1")
PY

addon=$(node -p 'process.platform+"-"+process.arch')
cp "$root/bindings/node/native/$addon/index.node" "$PLASMITE_WRONG_ABI/index.node"
DYLD_LIBRARY_PATH="$PLASMITE_WRONG_ABI" LD_LIBRARY_PATH="$PLASMITE_WRONG_ABI" \
  node - "$PLASMITE_WRONG_ABI/index.node" <<'JS'
const assert = require("node:assert/strict");
assert.throws(() => require(process.argv[2]), /Plasmite ABI major mismatch: expected 2, found 1/);
JS
