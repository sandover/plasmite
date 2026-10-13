/*
Purpose: C ABI for Plasmite bindings using libplasmite.
Key Exports: Client/Pool/Stream handles, JSON + Lite3 append/get/tail functions, buffers, errors.
Role: Stable boundary for official bindings (Go/Python/Node).

ABI stability:
  - Within a major version, this header is additive-only: no field removals,
    no reordering of struct members, no changes to enum discriminant values.
  - New functions and struct fields are appended at the end.
  - If a breaking change is unavoidable, it bumps the major version and the
    old symbols remain available via versioned symbol names (when supported).

Ownership rules:
  - Handles (plsm_client_t*, plsm_pool_t*, plsm_stream_t*) are opaque.
    The caller receives ownership from *_new / *_open / *_create and must
    pass the handle to the matching *_free when done.
  - Buffers (plsm_buf_t) returned via out-parameters are caller-owned.
    Free with plsm_buf_free.  Do not mix with malloc/free.
  - Errors (plsm_error_t*) returned via out_err are caller-owned.
    Free with plsm_error_free.  NULL means no error.
  - String fields inside plsm_error_t are owned by the error struct;
    they become invalid after plsm_error_free.
  - Every *_free function accepts NULL. plsm_buf_free and
    plsm_lite3_frame_free reset caller-owned output structs, so freeing the
    same output struct again is a no-op.

Caller obligations:
  - Serialize all calls on each opaque client, pool, or stream handle,
    including reads and free. UDP pool send/follow calls may overlap while the
    handle stays alive; wait for all calls before freeing it. UDP follow cancel
    may run during next; cancel and wait for active calls before close.
    Distinct handles may run concurrently. Do not use a freed handle.
  - Every non-NULL pointer and pointer/length pair must refer to valid,
    suitably aligned storage for the complete call. Handles and error pointers
    must come from this library and be passed to their matching free exactly
    once. Using a handle after free, freeing a handle/error twice, passing an
    arbitrary non-NULL address, or overstating a buffer/tag length is undefined
    behavior in the caller and is not converted into a Plasmite error.

Linking:
  - Dynamic: link against libplasmite.dylib / .so / .dll.
  - Static: link against libplasmite.a (include system libs: -lpthread -ldl -lm).
  - pkg-config: `pkg-config --cflags --libs plasmite` (available when installed
    via Homebrew tap or release SDK tarball).

Notes: All allocations returned must be freed by the caller via provided free functions.
Notes: Remote pool refs are not supported by this ABI; use CLI or HTTP API instead.
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
typedef struct plsm_lite3_stream plsm_lite3_stream_t;
typedef struct plsm_udp_client plsm_udp_client_t;
typedef struct plsm_udp_pool plsm_udp_pool_t;
typedef struct plsm_udp_follow plsm_udp_follow_t;

typedef enum plsm_error_kind {
    PLSM_ERROR_INTERNAL = 1,
    PLSM_ERROR_USAGE = 2,
    PLSM_ERROR_NOT_FOUND = 3,
    PLSM_ERROR_ALREADY_EXISTS = 4,
    PLSM_ERROR_BUSY = 5,
    PLSM_ERROR_PERMISSION = 6,
    PLSM_ERROR_CORRUPT = 7,
    PLSM_ERROR_IO = 8,
    PLSM_ERROR_RETENTION_GAP = 9
} plsm_error_kind_t;

typedef enum plsm_gap_policy {
    PLSM_GAP_CONTINUE = 0,
    PLSM_GAP_ERROR = 1
} plsm_gap_policy_t;

/*
 * Options for plsm_stream_open_ex and plsm_lite3_stream_open_ex.
 *
 * Zero-initialize this structure, set struct_size to sizeof(plsm_stream_options_t),
 * then set desired fields.  A newer caller may pass a larger structure to an older
 * library; the library reads only this v1 prefix.  reserved must remain zero.
 */
typedef struct plsm_stream_options {
    uint32_t struct_size;
    uint32_t gap_policy;
    uint64_t since_seq;
    uint64_t max_messages;
    uint64_t timeout_ms;
    uint32_t has_since;
    uint32_t has_max;
    uint32_t has_timeout;
    uint32_t reserved;
} plsm_stream_options_t;

typedef struct plsm_buf {
    uint8_t *data;
    size_t len;
} plsm_buf_t;

typedef enum plsm_udp_start_kind {
    PLSM_UDP_START_NEW = 0,
    PLSM_UDP_START_LAST = 1,
    PLSM_UDP_START_SINCE = 2,
    PLSM_UDP_START_SEQUENCE = 3
} plsm_udp_start_kind_t;

typedef enum plsm_udp_event_kind {
    PLSM_UDP_EVENT_FRAME = 1,
    PLSM_UDP_EVENT_GAP = 2,
    PLSM_UDP_EVENT_PROGRESS = 3
} plsm_udp_event_kind_t;

typedef enum plsm_udp_skip_reason {
    PLSM_UDP_SKIP_UNKNOWN = 0,
    PLSM_UDP_SKIP_OVERSIZED = 1,
    PLSM_UDP_SKIP_RETENTION = 2,
    PLSM_UDP_SKIP_PRESSURE = 3
} plsm_udp_skip_reason_t;

/* Initialize to zero. Free every event with plsm_udp_event_free after reading it. */
typedef struct plsm_udp_event {
    uint32_t kind;
    uint32_t reason;
    uint64_t seq;
    uint64_t timestamp_ns;
    uint64_t first;
    uint64_t last;
    uint64_t start_seq;
    uint64_t next_seq;
    uint64_t end_seq;
    uint8_t has_end;
    uint8_t terminal;
    uint8_t retention_gap;
    uint8_t reserved;
    plsm_buf_t payload;
} plsm_udp_event_t;

typedef struct plsm_lite3_frame {
    uint64_t seq;
    uint64_t timestamp_ns;
    uint32_t flags;
    plsm_buf_t payload;
} plsm_lite3_frame_t;

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
    const char **tags,
    size_t tags_len,
    uint32_t durability,
    plsm_buf_t *out_message,
    plsm_error_t **out_err);

int plsm_pool_append_lite3(
    plsm_pool_t *pool,
    const uint8_t *payload,
    size_t payload_len,
    uint32_t durability,
    uint64_t *out_seq,
    plsm_error_t **out_err);

int plsm_pool_get_json(
    plsm_pool_t *pool,
    uint64_t seq,
    plsm_buf_t *out_message,
    plsm_error_t **out_err);

int plsm_pool_get_lite3(
    plsm_pool_t *pool,
    uint64_t seq,
    plsm_lite3_frame_t *out_frame,
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

int plsm_stream_open_ex(
    plsm_pool_t *pool,
    const plsm_stream_options_t *options,
    plsm_stream_t **out_stream,
    plsm_error_t **out_err);

int plsm_stream_next(
    plsm_stream_t *stream,
    plsm_buf_t *out_message,
    plsm_error_t **out_err);

int plsm_lite3_stream_open(
    plsm_pool_t *pool,
    uint64_t since_seq,
    uint32_t has_since,
    uint64_t max_messages,
    uint32_t has_max,
    uint64_t timeout_ms,
    uint32_t has_timeout,
    plsm_lite3_stream_t **out_stream,
    plsm_error_t **out_err);

int plsm_lite3_stream_open_ex(
    plsm_pool_t *pool,
    const plsm_stream_options_t *options,
    plsm_lite3_stream_t **out_stream,
    plsm_error_t **out_err);

int plsm_lite3_stream_next(
    plsm_lite3_stream_t *stream,
    plsm_lite3_frame_t *out_frame,
    plsm_error_t **out_err);

void plsm_stream_free(plsm_stream_t *stream);
void plsm_lite3_stream_free(plsm_lite3_stream_t *stream);

void plsm_buf_free(plsm_buf_t *buf);
void plsm_lite3_frame_free(plsm_lite3_frame_t *frame);
void plsm_error_free(plsm_error_t *err);

/*
 * UDP uses one explicit endpoint and a separate client/pool/follow handle.
 * `pool_name` must be a named pool. Send calls report local validation and OS
 * send errors only; success does not confirm that the server stored a message.
 */
int plsm_udp_client_new(
    const char *endpoint,
    plsm_udp_client_t **out_client,
    plsm_error_t **out_err);
void plsm_udp_client_free(plsm_udp_client_t *client);
int plsm_udp_client_open_pool(
    plsm_udp_client_t *client,
    const char *pool_name,
    plsm_udp_pool_t **out_pool,
    plsm_error_t **out_err);
void plsm_udp_pool_free(plsm_udp_pool_t *pool);
int plsm_udp_pool_send_json(
    plsm_udp_pool_t *pool,
    const uint8_t *json_bytes,
    size_t json_len,
    const char **tags,
    size_t tags_len,
    plsm_error_t **out_err);
int plsm_udp_pool_send_lite3(
    plsm_udp_pool_t *pool,
    const uint8_t *payload,
    size_t payload_len,
    plsm_error_t **out_err);
int plsm_udp_pool_follow(
    plsm_udp_pool_t *pool,
    uint32_t start_kind,
    uint64_t start_value,
    uint32_t live,
    plsm_udp_follow_t **out_follow,
    plsm_error_t **out_err);

/*
 * plsm_udp_follow_next returns 1 for an event, 0 when `timeout_ms` expires,
 * 2 when a finite follow ends, 3 after a concurrent cancel, and -1 on error.
 * Use one reader per follow handle. Cancel may run during next; a wait checks
 * cancellation within 50 ms. Before close, cancel and wait for every active
 * call to return. Close releases the subscription and frees the handle.
 * Do not use a handle after close. A zero timeout polls without blocking.
 */
int plsm_udp_follow_next(
    plsm_udp_follow_t *follow,
    uint64_t timeout_ms,
    plsm_udp_event_t *out_event,
    plsm_error_t **out_err);
void plsm_udp_follow_cancel(plsm_udp_follow_t *follow);
void plsm_udp_follow_close(plsm_udp_follow_t *follow);
void plsm_udp_event_free(plsm_udp_event_t *event);
int plsm_udp_event_message(
    const plsm_udp_event_t *event,
    plsm_buf_t *out_message,
    plsm_error_t **out_err);

#ifdef __cplusplus
} // extern "C"
#endif

#endif // PLASMITE_H
