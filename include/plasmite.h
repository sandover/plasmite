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
    including reads and free. Local stream cancel may overlap a read; wait for
    the read before freeing. UDP pool send/follow calls may overlap while the
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
#include <stdbool.h>
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
    PLSM_UDP_EVENT_GAP = 2, /* Compatibility name: an explicit server SKIP. */
    PLSM_UDP_EVENT_PROGRESS = 3
} plsm_udp_event_kind_t;

typedef enum plsm_udp_skip_reason {
    PLSM_UDP_SKIP_UNKNOWN = 0, /* Reserved ABI value; invalid in wire SKIP packets. */
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

/* Numeric identity from the successful append itself. No allocation or free.
 * timestamp_ns is unsigned Unix time in nanoseconds, not a formatted date.
 * Output remains untouched on error. Use matching SDK/library versions.
 */
typedef struct plsm_append_identity {
    uint64_t seq;
    uint64_t timestamp_ns;
} plsm_append_identity_t;

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

/* pool_ref names resolve in the client's directory. Native path separators
 * select a file path; use ./bare.plasmite (or .\bare.plasmite on Windows)
 * for an explicit bare file. Windows drive-relative forms are unsupported. */
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

/* Identity-only variants preserve validation, durability and commit ordering.
 * They avoid constructing a full message receipt and never reread the ring.
 */
int plsm_pool_append_json_identity(plsm_pool_t *pool, const uint8_t *json_bytes,
    size_t json_len, const char *const *tags, size_t tags_len, uint32_t durability,
    plsm_append_identity_t *out_identity, plsm_error_t **out_err);
int plsm_pool_append_lite3_identity(plsm_pool_t *pool, const uint8_t *payload,
    size_t payload_len, uint32_t durability, plsm_append_identity_t *out_identity,
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
 * Follow delivers matching DATA in arrival order, including late messages and
 * duplicates. Only explicit server skips produce gap events. Progress describes
 * server attempts and never suppresses a later-arriving DATA packet.
 * Finite follows end on terminal progress after draining accepted events;
 * DATA still in flight may go unread. Completion does not confirm full delivery.
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

/*
 * Bounded local-stream waits: 1=message, 0=wait expired, 2=stream finished,
 * 3=cancelled, -1=error. Wait expiry retains the cursor and message count.
 * Zero polls once; the original open-time deadline still ends the stream.
 * The budget cannot interrupt a file operation already waiting on an OS lock.
 * Use one reader per handle. Cancel may overlap next/next_wait, is permanent,
 * and is observed within 50 ms while idle. Cancel and wait for active reads
 * before free. Output buffers belong to the caller only on status 1.
 * The legacy next functions retain their original 1/0/-1 results.
 */
int plsm_stream_next_wait(plsm_stream_t *stream, uint64_t timeout_ms,
                          plsm_buf_t *out_message, plsm_error_t **out_err);
int plsm_lite3_stream_next_wait(plsm_lite3_stream_t *stream, uint64_t timeout_ms,
                               plsm_lite3_frame_t *out_frame, plsm_error_t **out_err);
void plsm_stream_cancel(plsm_stream_t *stream);
void plsm_lite3_stream_cancel(plsm_lite3_stream_t *stream);

/* Coarse selection from data. Paths are JSON Pointers: /metrics/power,
 * /samples/0/name, or empty for data itself. ~0 means ~ and ~1 means /.
 * Missing members or traversal through a scalar produce MISSING. Array indices
 * use decimal digits without leading zeroes. OBJECT/ARRAY return their member
 * count in integer; other unused fields stay zero. STRING/BYTES refer to offset
 * and len inside out_bytes, which owns copies of only requested values.
 *
 * Supply count caller-owned slots and a zero-initialized out_bytes. Free the
 * bytes with plsm_buf_free; slots contain no pointers and need no native free.
 * Keep payload/paths immutable for the call. Selection validates the frame once
 * per call, accepts unaligned payload bytes, and leaves outputs untouched on
 * error. Returned bytes survive source mutation, pool/stream close and overwrite.
 */
typedef enum plsm_field_kind {
    PLSM_FIELD_MISSING = 0, PLSM_FIELD_NULL = 1, PLSM_FIELD_BOOL = 2,
    PLSM_FIELD_I64 = 3, PLSM_FIELD_F64 = 4, PLSM_FIELD_STRING = 5,
    PLSM_FIELD_BYTES = 6, PLSM_FIELD_OBJECT = 7, PLSM_FIELD_ARRAY = 8
} plsm_field_kind_t;
typedef struct plsm_field_value {
    uint32_t kind;
    uint32_t reserved;
    int64_t integer;
    double real;
    size_t offset;
    size_t len;
} plsm_field_value_t;
typedef struct plsm_selected_frame {
    uint64_t seq;
    uint64_t timestamp_ns;
    uint32_t flags;
    uint32_t reserved;
} plsm_selected_frame_t;
int plsm_lite3_select(const uint8_t *payload, size_t len,
    const char *const *paths, size_t count, plsm_field_value_t *out_values,
    plsm_buf_t *out_bytes, plsm_error_t **out_err);
/* Fetch and select in one call: only requested bytes cross the language boundary. */
int plsm_pool_get_fields(plsm_pool_t *pool, uint64_t seq,
    const char *const *paths, size_t count, plsm_field_value_t *out_values,
    plsm_buf_t *out_bytes, plsm_selected_frame_t *out_frame, plsm_error_t **out_err);

/* Additive immutable snapshot API. Owners retain four-byte-aligned private
 * bytes independently of pool/stream lifetime. Retain/release and immutable
 * reads may run concurrently on independently held references. Keep an owner
 * alive through every borrowed access; never alter or forge a value_ref.
 * from_lite3 copies caller bytes. get_view moves the one pool snapshot.
 * Value references and borrowed key/string/byte spans need no individual free.
 * Mutation requires copying. A zero owner represents an absent value.
 */
typedef struct plsm_message_view plsm_message_view_t;
typedef struct plsm_value_ref {
    const plsm_message_view_t *owner;
    size_t offset;
} plsm_value_ref_t;
typedef struct plsm_view_value {
    uint32_t kind; /* plsm_field_kind_t; zero means absent */
    int64_t integer;
    double real;
    const uint8_t *bytes;
    size_t len; /* bytes for strings/bytes, unique count for containers */
} plsm_view_value_t;
typedef struct plsm_view_entry {
    const uint8_t *key; /* NULL for array entries */
    size_t key_len;
    plsm_value_ref_t value;
} plsm_view_entry_t;
int plsm_view_from_lite3(const uint8_t *bytes, size_t len, uint64_t seq,
    uint64_t timestamp_ns, const plsm_message_view_t **out, plsm_error_t **err);
int plsm_pool_get_view(plsm_pool_t *pool, uint64_t seq,
    const plsm_message_view_t **out, plsm_error_t **err);
/* New view API: exact integer tokens must fit i64; rejects before publication. */
int plsm_pool_append_view_json(plsm_pool_t *pool, const uint8_t *json, size_t len,
    const char *const *tags, size_t tags_len, uint32_t durability,
    const plsm_message_view_t **out, plsm_error_t **err);
int plsm_pool_append_view(plsm_pool_t *pool, const plsm_message_view_t *source,
    uint32_t durability, const plsm_message_view_t **out, plsm_error_t **err);
void plsm_view_retain(const plsm_message_view_t *view);
void plsm_view_release(const plsm_message_view_t *view);
plsm_value_ref_t plsm_view_root(const plsm_message_view_t *view);
plsm_value_ref_t plsm_view_data(const plsm_message_view_t *view);
const uint8_t *plsm_view_bytes(const plsm_message_view_t *view, size_t *len);
/* Conservative whole-owner property: finite numbers, exact binary64 integers,
 * and no native bytes. False for NULL or an owner whose builder did not track it. */
bool plsm_view_json_native_safe(const plsm_message_view_t *view);
/* Four-aligned caller storage, borrowed only during this call. If capacity is
 * insufficient, success reports required size without writing any payload.
 * A retry may return NotFound after retention; no cursor advances here.
 * The copied bytes require validation before becoming an immutable view. */
int plsm_pool_get_into(plsm_pool_t *pool, uint64_t seq, uint8_t *destination,
    size_t capacity, size_t *required, uint64_t *timestamp_ns,
    uint32_t *flags, plsm_error_t **err);
uint64_t plsm_view_seq(const plsm_message_view_t *view);
uint64_t plsm_view_timestamp_ns(const plsm_message_view_t *view);
/* Reserved payload storage capacity, excluding owner/index metadata. Shared
 * children/forwarded owners retain the same storage; NULL returns zero. */
size_t plsm_view_retained_bytes(const plsm_message_view_t *view);
int plsm_value_get(plsm_value_ref_t value, const uint8_t *key, size_t len,
    plsm_value_ref_t *out, plsm_error_t **err);
int plsm_value_index(plsm_value_ref_t value, uint32_t index,
    plsm_value_ref_t *out, plsm_error_t **err);
int plsm_value_read(plsm_value_ref_t value, plsm_view_value_t *out, plsm_error_t **err);
/* out_count always receives required capacity; insufficient capacity returns
 * Usage without filling slots. Keys use unique canonical last-entry values. */
int plsm_value_entries(plsm_value_ref_t value, plsm_view_entry_t *slots,
    size_t capacity, size_t *out_count, plsm_error_t **err);
/* Sequential cursor retains one owner and bounded state. Serialize calls on
 * each cursor. next fills at most capacity slots; zero written means end when
 * capacity > 0. No collection-sized entry array or full scan per chunk. */
typedef struct PlsmViewCursor plsm_view_cursor_t;
int plsm_value_iter(plsm_value_ref_t value, plsm_view_cursor_t **out, plsm_error_t **err);
int plsm_cursor_next(plsm_view_cursor_t *cursor, plsm_view_entry_t *slots,
    size_t capacity, size_t *written, plsm_error_t **err);
void plsm_cursor_free(plsm_view_cursor_t *cursor);
/* Compile JSON Pointers once; reuse without parsing paths or validating the
 * immutable message again. Selection returns borrowed refs, no string copies.
 * Keep selector immutable and alive during calls; free it independently. */
typedef struct PlsmSelector plsm_selector_t;
int plsm_selector_new(const char *const *paths, size_t count,
    plsm_selector_t **out, plsm_error_t **err);
void plsm_selector_free(plsm_selector_t *selector);
int plsm_view_select(const plsm_message_view_t *view,
    const plsm_selector_t *selector, plsm_value_ref_t *slots,
    size_t capacity, plsm_error_t **err);
int plsm_value_to_json(plsm_value_ref_t value, plsm_buf_t *out, plsm_error_t **err);
/* Native-copy helper preserves view key order; normal JSON output stays canonical. */
int plsm_value_to_json_stored(plsm_value_ref_t value, plsm_buf_t *out, plsm_error_t **err);
/* Complete seq/time/meta/data envelope, with no intermediate native object tree. */
int plsm_view_to_json(const plsm_message_view_t *view, plsm_buf_t *out, plsm_error_t **err);

/* Native input walkers. All calls on a builder require caller serialization.
 * Text spans borrow input for the call only; embedded NUL is valid in values
 * and tags, but object keys reject it. NULL key means an array element; a
 * non-NULL empty span names the empty object key. Kinds match plsm_view_value_t.
 * Nodes are opaque handles, not byte offsets; pass them back unchanged and
 * use only nodes returned by this builder. Container puts return a child node.
 * append_builder consumes the contents on both success and failure after input
 * guards; free the handle once afterward. No publication occurs on build errors.
 * A flush error can follow publication, as for other appends. */
typedef struct plsm_builder plsm_builder_t;
typedef struct { const uint8_t *data; size_t len; } plsm_text_t;
int plsm_builder_new(const plsm_text_t *tags, size_t count,
    plsm_builder_t **out, size_t *data_node, plsm_error_t **err);
int plsm_builder_put(plsm_builder_t *builder, size_t parent,
    const uint8_t *key, size_t key_len, const plsm_view_value_t *value,
    size_t *child_node, plsm_error_t **err);
int plsm_pool_append_builder(plsm_pool_t *pool, plsm_builder_t *builder,
    uint32_t durability, const plsm_message_view_t **out, plsm_error_t **err);
/* Copy a checked subtree while the source owner remains alive. SIZE_MAX parent
 * fills an empty data object from a source object and requires NULL key/zero
 * length. It preserves the data_node returned by builder_new; other parents use
 * the ordinary object-key/array-item rule. No host scalar conversion occurs. */
int plsm_builder_copy_value(plsm_builder_t *builder, size_t parent,
    const uint8_t *key, size_t key_len, plsm_value_ref_t source, plsm_error_t **err);
void plsm_builder_free(plsm_builder_t *builder);

#ifdef __cplusplus
} // extern "C"
#endif

#endif // PLASMITE_H
