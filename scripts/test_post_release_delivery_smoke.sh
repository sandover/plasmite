#!/usr/bin/env bash
# Purpose: Verify delivery checks fail closed without accessing package registries.
# Role: Exercise the real smoke runner with deterministic package-manager fixtures.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runner="${PLASMITE_SMOKE_RUNNER:-$root_dir/scripts/post_release_delivery_smoke.sh}"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
mkdir -p "$tmp_dir/bin"

cat > "$tmp_dir/bin/plasmite-fixture" <<'CLI'
#!/usr/bin/env bash
case "$PWD" in
  */npm) channel=npm ;;
  */pnpm) channel=pnpm ;;
  *) channel=crates ;;
esac
if [[ "$FAKE_FAIL" == "$channel-version" ]]; then
  echo 'plasmite 1.0.00'
else
  echo 'plasmite 1.0.0'
fi
[[ "$FAKE_FAIL" != "$channel-runtime" ]]
CLI

cat > "$tmp_dir/bin/package-manager" <<'MANAGER'
#!/usr/bin/env bash
set -euo pipefail
tool="${0##*/}"
printf '%s %s\n' "$tool" "$*" >> "$TRACE"
case "$tool" in
  npm|pnpm)
    if [[ "$1" == init ]]; then
      [[ "$FAKE_FAIL" != npm-init ]]
      exit
    fi
    mkdir -p node_modules/.bin
    cp "$FIXTURE_BIN/plasmite-fixture" node_modules/.bin/plasmite
    # Leave a working CLI even when installation fails: a subsequent successful
    # runtime or pnpm leg must never overwrite the failed install's status.
    [[ "$FAKE_FAIL" != "$tool-install" ]] || exit 42
    if [[ "$FAKE_FAIL" == npm-once && "$tool" == npm ]] &&
       [[ "$(grep -c '^npm install ' "$TRACE")" == 1 ]]; then
      exit 42
    fi
    ;;
  cargo)
    while [[ "$1" != --root ]]; do shift; done
    mkdir -p "$2/bin"
    cp "$FIXTURE_BIN/plasmite-fixture" "$2/bin/plasmite"
    [[ "$FAKE_FAIL" != crates-install ]] || exit 42
    ;;
  uv)
    if [[ "$FAKE_FAIL" == pypi-version ]]; then
      echo 'plasmite 1.0.00'
    else
      echo 'plasmite 1.0.0'
    fi
    [[ "$FAKE_FAIL" != pypi-runtime ]] || exit 42
    ;;
  sleep) ;; # Retries use the real loop without waiting for registry propagation.
esac
MANAGER
chmod +x "$tmp_dir/bin/plasmite-fixture" "$tmp_dir/bin/package-manager"
for tool in npm pnpm cargo uv sleep; do
  ln -s package-manager "$tmp_dir/bin/$tool"
done

run_case() {
  local failure="$1" channels="$2" expected="$3" budget="${4:-0}"
  local workdir="$tmp_dir/$failure" status
  mkdir -p "$workdir"
  if (
    cd "$workdir"
    PATH="$tmp_dir/bin:$PATH" FIXTURE_BIN="$tmp_dir/bin" \
      FAKE_FAIL="$failure" TRACE="$workdir/trace" \
      /bin/bash "$runner" --version 1.0.0 --channels "$channels" \
        --max-wait-minutes "$budget"
  ) > "$workdir/output" 2>&1; then
    status=0
  else
    status=$?
  fi
  if [[ "$status" != "$expected" ]]; then
    cat "$workdir/output" >&2
    echo "error: $failure exited $status, expected $expected" >&2
    exit 1
  fi
}

run_case success npm,pypi,crates 0
for failure in npm-install npm-init npm-runtime npm-version pnpm-install pnpm-runtime pnpm-version; do
  run_case "$failure" npm 1
done
if grep -q '^pnpm ' "$tmp_dir/npm-install/trace"; then
  echo 'error: failed npm installation reached the successful pnpm leg' >&2
  exit 1
fi
for failure in crates-install crates-runtime crates-version; do
  run_case "$failure" crates 1
done
for failure in pypi-runtime pypi-version; do
  run_case "$failure" pypi 1
done
run_case npm-once npm 0 1
grep -q '^\[npm\] attempt 2$' "$tmp_dir/npm-once/output"
grep -q '^pnpm add ' "$tmp_dir/npm-once/trace"
echo 'post-release delivery smoke fixtures ok (14 cases)'
