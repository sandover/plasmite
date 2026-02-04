/*
Purpose: C ABI for Plasmite bindings using libplasmite.
Key Exports: Client/Pool/Stream handles, append/get/tail functions, buffers, errors.
Role: Stable boundary for official bindings (Go/Python/Node) in v0.
Invariants: JSON bytes in/out; opaque handles; explicit free functions.
Invariants: Error kinds are stable; remote refs return Usage in v0.
Notes: All allocations returned must be freed by the caller via provided free functions.
*/

#ifndef PLASMITE_H
#define PLASMITE_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct plsm_client plsm_client_t;
typedef struct plsm_pool plsm_pool_t;
typedef struct plsm_stream plsm_stream_t;

typedef enum plsm_error_kind {
    PLSM_ERROR_INTERNAL = 1,
    PLSM_ERROR_USAGE = 2,
    PLSM_ERROR_NOT_FOUND = 3,
    PLSM_ERROR_ALREADY_EXISTS = 4,
    PLSM_ERROR_BUSY = 5,
    PLSM_ERROR_PERMISSION = 6,
    PLSM_ERROR_CORRUPT = 7,
    PLSM_ERROR_IO = 8
} plsm_error_kind_t;

typedef struct plsm_buf {
    uint8_t *data;
    size_t len;
} plsm_buf_t;

typedef struct plsm_error {
    int32_t kind;
    char *message;
    char *path;
    uint64_t seq;
    uint64_t offset;
    uint8_t has_seq;
    uint8_t has_offset;
} plsm_error_t;

int plsm_client_new(const char *pool_dir, plsm_client_t **out_client, plsm_error_t **out_err);
void plsm_client_free(plsm_client_t *client);

int plsm_pool_create(plsm_client_t *client, const char *pool_ref, uint64_t size_bytes, plsm_pool_t **out_pool, plsm_error_t **out_err);
int plsm_pool_open(plsm_client_t *client, const char *pool_ref, plsm_pool_t **out_pool, plsm_error_t **out_err);
void plsm_pool_free(plsm_pool_t *pool);

int plsm_pool_append_json(
    plsm_pool_t *pool,
    const uint8_t *json_bytes,
    size_t json_len,
    const char **descrips,
    size_t descrips_len,
    uint32_t durability,
    plsm_buf_t *out_message,
    plsm_error_t **out_err);

int plsm_pool_get_json(
    plsm_pool_t *pool,
    uint64_t seq,
    plsm_buf_t *out_message,
    plsm_error_t **out_err);

int plsm_stream_open(
    plsm_pool_t *pool,
    uint64_t since_seq,
    uint32_t has_since,
    uint64_t max_messages,
    uint32_t has_max,
    uint64_t timeout_ms,
    uint32_t has_timeout,
    plsm_stream_t **out_stream,
    plsm_error_t **out_err);

int plsm_stream_next(
    plsm_stream_t *stream,
    plsm_buf_t *out_message,
    plsm_error_t **out_err);

void plsm_stream_free(plsm_stream_t *stream);

void plsm_buf_free(plsm_buf_t *buf);
void plsm_error_free(plsm_error_t *err);

#ifdef __cplusplus
} // extern "C"
#endif

#endif // PLASMITE_H
