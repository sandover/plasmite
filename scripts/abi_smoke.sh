#!/usr/bin/env bash
# Purpose: Build+link smoke test for the libplasmite C ABI.
# Exports: None (script only).
# Role: CI/local guard against ABI drift or missing symbols.
# Invariants: Uses repo-local include/ and target/ outputs.
# Notes: Writes temp artifacts under .scratch/.
# Notes: Expects libplasmite built in target/debug or target/release.
# Notes: To test mixed versions, also pass a checkout of v1.3.0 built with cargo build --lib.

set -euo pipefail

root_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root_dir"

build_dir=${1:-"$root_dir/target/debug"}
if [[ ! -d "$build_dir" ]]; then
  echo "build directory not found: $build_dir" >&2
  exit 1
fi

mkdir -p .scratch
work_dir=$(mktemp -d .scratch/abi-smoke.XXXXXX)
trap 'rm -rf "$work_dir"' EXIT
mkdir -p "$work_dir/pools"

cat > "$work_dir/abi_smoke.c" <<'C'
#include "plasmite.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int fail(const char *label, plsm_error_t *err) {
    fprintf(stderr, "%s failed", label);
    if (err && err->message) {
        fprintf(stderr, ": %s", err->message);
    }
    fprintf(stderr, "\n");
    if (err) {
        plsm_error_free(err);
    }
    return 1;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <pool_dir>\n", argv[0]);
        return 1;
    }

    plsm_client_t *client = NULL;
    plsm_pool_t *pool = NULL;
    plsm_error_t *err = NULL;

    if (plsm_abi_major() != PLSM_ABI_MAJOR) {
        fprintf(stderr, "Plasmite ABI major %u required, loaded %u\n",
                PLSM_ABI_MAJOR, plsm_abi_major());
        return 1;
    }

    if (plsm_client_new(argv[1], &client, &err) != 0) {
        return fail("plsm_client_new", err);
    }

    if (plsm_pool_create(client, "smoke", 4 * 1024 * 1024, &pool, &err) != 0) {
        plsm_client_free(client);
        return fail("plsm_pool_create", err);
    }

    const plsm_text_t tags[] = {{(const uint8_t *)"smoke", 5}};
    const char *json = "{\"kind\":\"smoke\"}";
    plsm_buf_t buf = {0};
    const plsm_feed_options_t feed = {
        .struct_size = sizeof(feed),
        .tags = tags,
        .tags_len = 1,
    };
    if (plsm_pool_feed_json(
            pool,
            (const uint8_t *)json,
            strlen(json),
            &feed,
            &buf,
            &err) != 0) {
        plsm_pool_free(pool);
        plsm_client_free(client);
        return fail("plsm_pool_feed_json", err);
    }

    plsm_buf_free(&buf);
    plsm_pool_free(pool);
    plsm_client_free(client);
    return 0;
}
C

cc -I "$root_dir/include" \
  "$work_dir/abi_smoke.c" \
  -L "$build_dir" -lplasmite \
  -o "$work_dir/abi_smoke"

if [[ "$(uname)" == "Darwin" ]]; then
  old_name=libplasmite.dylib
  new_name=libplasmite.2.dylib
  search_path=DYLD_LIBRARY_PATH
else
  old_name=libplasmite.so
  new_name=libplasmite.so.2
  search_path=LD_LIBRARY_PATH
fi

cp "$build_dir/$old_name" "$work_dir/$new_name"
env "$search_path=$work_dir" "$work_dir/abi_smoke" "$work_dir/pools"

if [[ $# -ge 2 ]]; then
  baseline_dir="$2"
  mkdir "$work_dir/baseline" "$work_dir/baseline-pools"
  cp "$baseline_dir/target/debug/$old_name" "$work_dir/baseline/$old_name"
  cat > "$work_dir/baseline.c" <<'C'
#include "plasmite.h"

int main(int argc, char **argv) {
    plsm_client_t *client;
    if (argc != 2 || plsm_client_new(argv[1], &client, NULL) != 0) {
        return 1;
    }
    plsm_client_free(client);
    return 0;
}
C
  cc -I "$baseline_dir/include" "$work_dir/baseline.c" \
    "$work_dir/baseline/$old_name" -o "$work_dir/baseline-client"
  env "$search_path=$work_dir/baseline" "$work_dir/baseline-client" "$work_dir/baseline-pools"

  reject_library() {
    local binary="$1" dir="$2" expected_name="$3"
    if env "$search_path=$dir" "$binary" "$work_dir/baseline-pools" > "$work_dir/loader.log" 2>&1; then
      echo "error: $binary loaded the wrong major version" >&2
      return 1
    fi
    if ! rg --fixed-strings --quiet "$expected_name" "$work_dir/loader.log"; then
      cat "$work_dir/loader.log" >&2
      echo "error: loader did not name the required library $expected_name" >&2
      return 1
    fi
  }

  reject_library "$work_dir/baseline-client" "$work_dir" "$old_name"
  reject_library "$work_dir/abi_smoke" "$work_dir/baseline" "$new_name"
  echo "C ABI loader rejected mismatched library majors in both directions"
fi
