#!/usr/bin/env bash
# Purpose: Build+link smoke test for the libplasmite C ABI.
# Exports: None (script only).
# Role: CI/local guard against ABI drift or missing symbols.
# Invariants: Uses repo-local include/ and target/ outputs.
# Notes: Writes temp artifacts under .scratch/.
# Notes: Expects libplasmite built in target/debug or target/release.

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

    if (plsm_client_new(argv[1], &client, &err) != 0) {
        return fail("plsm_client_new", err);
    }

    if (plsm_pool_create(client, "smoke", 4 * 1024 * 1024, &pool, &err) != 0) {
        plsm_client_free(client);
        return fail("plsm_pool_create", err);
    }

    const char *tags[] = {"smoke"};
    const char *json = "{\"kind\":\"smoke\"}";
    plsm_buf_t buf = {0};
    if (plsm_pool_append_json(
            pool,
            (const uint8_t *)json,
            strlen(json),
            tags,
            1,
            0,
            &buf,
            &err) != 0) {
        plsm_pool_free(pool);
        plsm_client_free(client);
        return fail("plsm_pool_append_json", err);
    }

    plsm_buf_free(&buf);
    plsm_pool_free(pool);
    plsm_client_free(client);
    return 0;
}
C

cc -I "$root_dir/include" \
  -L "$build_dir" -lplasmite \
  "$work_dir/abi_smoke.c" -o "$work_dir/abi_smoke"

if [[ "$(uname)" == "Darwin" ]]; then
  DYLD_LIBRARY_PATH="$build_dir" "$work_dir/abi_smoke" "$work_dir/pools"
else
  LD_LIBRARY_PATH="$build_dir" "$work_dir/abi_smoke" "$work_dir/pools"
fi
