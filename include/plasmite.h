/*
Plasmite C ABI, major version 2.

Plasmite stores JSON messages in pools: bounded ring buffers on disk that many
processes can write and read at once. Feed writes a message, follow waits for
new ones, and fetch reads what the pool holds and stops. A pool reference is a
name in the client's directory, a file path, or an https:// URL on a server
saved with `plasmite access connect`. The Go, Python and Node bindings wrap
this library; spec/v2/SPEC.md in the Plasmite repository holds the contract.

Return values:
  - Functions that return int return 0 on success and -1 on failure. On
    failure they store a new plsm_error_t in *out_err. Pass NULL for out_err
    to get the -1 without details.
  - Next calls (plsm_stream_next_message, plsm_udp_follow_next_view)
    return 1 for an item, 0 when timeout_ms passes, 2 when a finite read ends,
    3 after a cancel, and -1 on error. A timeout leaves the reader open.
    timeout_ms 0 polls once; UINT64_MAX waits without a limit.
  - A PLSM_ERROR_RETENTION_GAP error means the pool overwrote messages before
    the reader reached them. `missing` holds the first missed sequence and
    `next_seq` the oldest sequence still retained. Reopen at next_seq to
    accept the loss.

Handles and their release calls. Every release call accepts NULL; never use a
handle after releasing it.
  plsm_client_t        plsm_client_new                   plsm_client_free
  plsm_pool_t          plsm_pool_open, plsm_pool_create  plsm_pool_free
  plsm_stream_t        plsm_pool_follow, plsm_pool_fetch plsm_stream_free
  plsm_udp_client_t    plsm_udp_client_new               plsm_udp_client_free
  plsm_udp_pool_t      plsm_udp_client_open_pool         plsm_udp_pool_free
  plsm_udp_follow_t    plsm_udp_pool_follow/fetch        plsm_udp_follow_close
  plsm_message_t       plsm_pool_feed and reader items   plsm_message_release
  plsm_view_cursor_t   plsm_value_iter                   plsm_cursor_free
  plsm_selector_t      plsm_selector_new                 plsm_selector_free
  plsm_builder_t       plsm_builder_new                  plsm_builder_free
  plsm_error_t         any failing call                  plsm_error_free

Buffers and outputs:
  - Initialize output structs to zero. Free a successful plsm_buf_t with
    plsm_buf_free and a UDP event with plsm_udp_event_free.
  - A stored message owns its encoded payload. Its borrowed spans stay valid
    until the last owner releases it, including after reader and pool closure.
  - plsm_pool_info and plsm_pool_list return JSON describing pools.
  - Strings inside plsm_error_t belong to the error and go away with
    plsm_error_free. Free any previous error before reusing its output slot.

Example: print every retained message from a saved sequence onward.

    plsm_client_t *client = NULL;
    plsm_pool_t *pool = NULL;
    plsm_stream_t *stream = NULL;
    plsm_error_t *err = NULL;
    plsm_read_options_t from = { sizeof from, PLSM_READ_SEQUENCE, checkpoint, NULL, 0 };

    if (plsm_client_new("./data", &client, &err) == 0 &&
        plsm_pool_open(client, "sensors", 1, &pool, &err) == 0 &&
        plsm_pool_fetch(pool, &from, &stream, &err) == 0) {
        for (;;) {
            const plsm_message_t *message = NULL;
            int status = plsm_stream_next_message(stream, 1000, &message, &err);
            if (status == 1) {
                printf("message %llu\n", (unsigned long long)plsm_message_seq(message));
                plsm_message_release(message);
            } else if (status != 0) {
                break;  // 2 finished, 3 cancelled, -1 failed with err set
            }
        }
    }
    if (err && err->kind == PLSM_ERROR_RETENTION_GAP)
        fprintf(stderr, "lost from %llu; reopen at %llu\n",
                (unsigned long long)err->missing, (unsigned long long)err->next_seq);
    else if (err)
        fprintf(stderr, "plasmite: %s\n", err->message);
    plsm_error_free(err);
    plsm_stream_free(stream);
    plsm_pool_free(pool);
    plsm_client_free(client);

Use plsm_pool_follow instead of plsm_pool_fetch to keep waiting for new
messages.

Threads:
  - Serialize all calls on each client, pool or stream handle, including
    reads and free. Separate handles may run concurrently.
  - A local stream cancel may overlap a read; wait for the read to return
    before freeing the stream.
  - UDP pool send and read calls may overlap while the handle stays alive;
    wait for all of them before freeing it. A UDP follow cancel may run during
    next; cancel and wait for active calls before close.

Caller obligations: every non-NULL pointer and pointer/length pair must refer
to valid, suitably aligned storage for the whole call. Handles and errors must
come from this library and go to their release call exactly once. Using a
handle after release, releasing it twice, passing an arbitrary non-NULL
address or overstating a buffer or tag length is undefined behavior; Plasmite
cannot turn it into an error.

ABI stability: within a major version this header only grows. Struct members
keep their order, enum values keep their numbers, and new functions and
fields go at the end. A breaking change takes a new major version, and each
major version has its own library file name.

Linking:
  - Dynamic: libplasmite.2.dylib, libplasmite.so.2 or plasmite2.dll.
  - Static: libplasmite.a, plus the system libraries -lpthread -ldl -lm.
  - pkg-config: `pkg-config --cflags --libs plasmite` works after a Homebrew
    or release SDK install.
*/

#ifndef PLASMITE_H
#define PLASMITE_H

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>

#define PLSM_ABI_MAJOR 2

#ifdef __cplusplus
extern "C" {
#endif

#ifdef PLSM_INTERNAL_ABI
#define plsm_lite3_frame_free plsm_internal_lite3_frame_free
#define plsm_lite3_stream_cancel plsm_internal_lite3_stream_cancel
#define plsm_lite3_stream_free plsm_internal_lite3_stream_free
#define plsm_lite3_stream_next_wait plsm_internal_lite3_stream_next_wait
#define plsm_pool_feed_builder_view plsm_internal_pool_feed_builder_view
#define plsm_pool_feed_json plsm_internal_pool_feed_json
#define plsm_pool_feed_json_receipt plsm_internal_pool_feed_json_receipt
#define plsm_pool_feed_json_view plsm_internal_pool_feed_json_view
#define plsm_pool_feed_lite3 plsm_internal_pool_feed_lite3
#define plsm_pool_feed_lite3_receipt plsm_internal_pool_feed_lite3_receipt
#define plsm_pool_feed_view plsm_internal_pool_feed_view
#define plsm_pool_fetch_lite3 plsm_internal_pool_fetch_lite3
#define plsm_pool_follow_lite3 plsm_internal_pool_follow_lite3
#define plsm_stream_next_wait plsm_internal_stream_next_wait
#define plsm_udp_event_message plsm_internal_udp_event_message
#define plsm_udp_follow_next plsm_internal_udp_follow_next
#define plsm_udp_pool_feed_json plsm_internal_udp_pool_feed_json
#define plsm_udp_pool_feed_lite3 plsm_internal_udp_pool_feed_lite3
#define plsm_udp_pool_read plsm_internal_udp_pool_read
#define plsm_view_stream_next plsm_internal_view_stream_next
#endif

uint32_t plsm_abi_major(void);

typedef struct plsm_client plsm_client_t;
typedef struct plsm_pool plsm_pool_t;
typedef struct plsm_stream plsm_stream_t;
#ifdef PLSM_INTERNAL_ABI
typedef struct plsm_lite3_stream plsm_lite3_stream_t;
#endif
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

typedef struct plsm_buf {
    uint8_t *data;
    size_t len;
} plsm_buf_t;

typedef enum plsm_udp_event_kind {
    PLSM_UDP_EVENT_FRAME = 1,
    PLSM_UDP_EVENT_SKIP = 2,
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
    uint64_t time;
    uint64_t first;
    uint64_t last;
    uint64_t start_seq;
    uint64_t next_seq;
    uint64_t end_seq;
    uint8_t has_end;
    uint8_t terminal;
    uint8_t reserved;
    plsm_buf_t payload;
} plsm_udp_event_t;

#ifdef PLSM_INTERNAL_ABI
/* A caller-owned receipt from a successful feed. Time counts Unix nanoseconds.
 * A failed call leaves the output untouched. */
typedef struct plsm_feed_receipt {
    uint64_t seq;
    uint64_t time;
} plsm_feed_receipt_t;

typedef struct plsm_lite3_frame {
    uint64_t seq;
    uint64_t time;
    uint32_t flags;
    plsm_buf_t payload;
} plsm_lite3_frame_t;
#endif

typedef struct plsm_error {
    int32_t kind;
    char *message;
    char *path;
    uint64_t seq;
    uint64_t offset;
    uint8_t has_seq;
    uint8_t has_offset;
    uint64_t missing;  /* For a retention gap. Zero otherwise. */
    uint64_t next_seq; /* Oldest retained at gap detection. Zero otherwise. */
} plsm_error_t;

typedef struct { const uint8_t *data; size_t len; } plsm_text_t;

/* Read and feed options (plsm_read_options_t, plsm_feed_options_t): pass NULL
 * for defaults, or set struct_size to sizeof the struct. Tag arrays and their
 * strings must stay valid for the call; a stream keeps its own copy.
 * start_value means: SEQUENCE, the first sequence to read; TIME, Unix
 * nanoseconds; AGO, nanoseconds before now; LAST, a message count. DEFAULT
 * starts a follow at the next new message and a fetch at the oldest. */
typedef enum plsm_read_start_kind {
    PLSM_READ_DEFAULT = 0,
    PLSM_READ_OLDEST = 1,
    PLSM_READ_SEQUENCE = 2,
    PLSM_READ_TIME = 3,
    PLSM_READ_AGO = 4,
    PLSM_READ_LAST = 5
} plsm_read_start_kind_t;

typedef struct plsm_read_options {
    uint32_t struct_size;
    uint32_t start_kind;
    uint64_t start_value; /* Time and duration are in nanoseconds. */
    const plsm_text_t *tags;
    size_t tags_len;
} plsm_read_options_t;

typedef struct plsm_feed_options {
    uint32_t struct_size;
    uint32_t durability; /* 0=fast, 1=flush */
    const plsm_text_t *tags;
    size_t tags_len;
} plsm_feed_options_t;

/* Inspect one pool or list the client's pools as a JSON object or array.
 * Free successful results with plsm_buf_free. Retained bounds can go stale
 * immediately after these calls return. */
int plsm_pool_info(plsm_pool_t *, plsm_buf_t *, plsm_error_t **);
/* Description JSON is an object with optional about and example fields. Set
 * replaces both fields. See the pool size and ownership rules for create. */
int plsm_pool_create_described(plsm_client_t *, const char *, uint64_t,
                              const uint8_t *, size_t, plsm_pool_t **, plsm_error_t **);
int plsm_pool_set_description(plsm_pool_t *, const uint8_t *, size_t, plsm_error_t **);
int plsm_pool_list(plsm_client_t *, plsm_buf_t *, plsm_error_t **);
int plsm_pool_delete(plsm_client_t *, const char *, plsm_error_t **);

#ifdef PLSM_INTERNAL_ABI
int plsm_pool_feed_json(plsm_pool_t *, const uint8_t *, size_t,
    const plsm_feed_options_t *, plsm_buf_t *, plsm_error_t **);
int plsm_pool_feed_lite3(plsm_pool_t *, const uint8_t *, size_t,
    const plsm_feed_options_t *, plsm_buf_t *, plsm_error_t **);
int plsm_pool_feed_json_receipt(plsm_pool_t *, const uint8_t *, size_t,
    const plsm_feed_options_t *, plsm_feed_receipt_t *, plsm_error_t **);
int plsm_pool_feed_lite3_receipt(plsm_pool_t *, const uint8_t *, size_t,
    const plsm_feed_options_t *, plsm_feed_receipt_t *, plsm_error_t **);
#endif
int plsm_pool_fetch(plsm_pool_t *, const plsm_read_options_t *, plsm_stream_t **, plsm_error_t **);
#ifdef PLSM_INTERNAL_ABI
int plsm_pool_fetch_lite3(plsm_pool_t *, const plsm_read_options_t *, plsm_lite3_stream_t **, plsm_error_t **);
#endif
/* Pass create=1 to make a 1 MiB pool if it does not exist, or 0 to open only. */
int plsm_pool_open(plsm_client_t *, const char *, uint32_t create, plsm_pool_t **, plsm_error_t **);
int plsm_pool_follow(plsm_pool_t *, const plsm_read_options_t *, plsm_stream_t **, plsm_error_t **);
#ifdef PLSM_INTERNAL_ABI
int plsm_pool_follow_lite3(plsm_pool_t *, const plsm_read_options_t *, plsm_lite3_stream_t **, plsm_error_t **);
#endif

/* A NULL pool_dir uses the default directory, ~/.plasmite/pools. */
int plsm_client_new(const char *pool_dir, plsm_client_t **out_client, plsm_error_t **out_err);
void plsm_client_free(plsm_client_t *client);

/* pool_ref names resolve in the client's directory. Native path separators
 * select a file path; use ./bare.plasmite (or .\bare.plasmite on Windows)
 * for an explicit bare file. Windows drive-relative forms are unsupported. */
int plsm_pool_create(plsm_client_t *client, const char *pool_ref, uint64_t size_bytes, plsm_pool_t **out_pool, plsm_error_t **out_err);
void plsm_pool_free(plsm_pool_t *pool);

void plsm_stream_free(plsm_stream_t *stream);
#ifdef PLSM_INTERNAL_ABI
void plsm_lite3_stream_free(plsm_lite3_stream_t *stream);
#endif

void plsm_buf_free(plsm_buf_t *buf);
#ifdef PLSM_INTERNAL_ABI
void plsm_lite3_frame_free(plsm_lite3_frame_t *frame);
#endif
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
#ifdef PLSM_INTERNAL_ABI
/* UDP feed confirms local socket admission. It has no storage receipt. */
int plsm_udp_pool_feed_json(plsm_udp_pool_t *, const uint8_t *, size_t,
    const plsm_feed_options_t *, plsm_error_t **);
/* Pass live=1 for follow or zero for fetch. Pass views=1 to receive owned
 * views through plsm_udp_follow_next_view. The same cancel/close rules apply. */
int plsm_udp_pool_read(plsm_udp_pool_t *, const plsm_read_options_t *, uint32_t live,
    uint32_t views, plsm_udp_follow_t **, plsm_error_t **);
int plsm_udp_pool_feed_lite3(
    plsm_udp_pool_t *pool,
    const uint8_t *payload,
    size_t payload_len,
    const plsm_feed_options_t *options,
    plsm_error_t **out_err);
/*
 * plsm_udp_follow_next returns 1 for an event, 0 when `timeout_ms` expires,
 * 2 when a finite follow ends, 3 after a concurrent cancel, and -1 on error.
 * Use one reader per follow handle. Cancel may run during next; a wait checks
 * cancellation within 50 ms. Before close, cancel and wait for every active
 * call to return. Close releases the subscription and frees the handle.
 * Zero polls once. UINT64_MAX waits without a time limit.
 */
int plsm_udp_follow_next(
    plsm_udp_follow_t *follow,
    uint64_t timeout_ms,
    plsm_udp_event_t *out_event,
    plsm_error_t **out_err);
#endif
void plsm_udp_follow_cancel(plsm_udp_follow_t *follow);
void plsm_udp_follow_close(plsm_udp_follow_t *follow);
void plsm_udp_event_free(plsm_udp_event_t *event);
#ifdef PLSM_INTERNAL_ABI
int plsm_udp_event_message(
    const plsm_udp_event_t *event,
    plsm_buf_t *out_message,
    plsm_error_t **out_err);
#endif

/*
 * Reader waits: 1=message, 0=wait expired, 2=reader finished, 3=cancelled,
 * -1=error. A timeout leaves the reader open. Zero polls once; UINT64_MAX
 * waits without a time limit. A read failure remains visible on later calls.
 * The wait cannot interrupt a file operation already waiting on an OS lock.
 * Cancel may overlap a read and is permanent. Wait for the read before free.
 * Output buffers belong to the caller only on status 1.
 */
#ifdef PLSM_INTERNAL_ABI
int plsm_stream_next_wait(plsm_stream_t *stream, uint64_t timeout_ms,
                          plsm_buf_t *out_message, plsm_error_t **out_err);
int plsm_lite3_stream_next_wait(plsm_lite3_stream_t *stream, uint64_t timeout_ms,
                               plsm_lite3_frame_t *out_frame, plsm_error_t **out_err);
#endif
void plsm_stream_cancel(plsm_stream_t *stream);
#ifdef PLSM_INTERNAL_ABI
void plsm_lite3_stream_cancel(plsm_lite3_stream_t *stream);
#endif

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
int plsm_lite3_select(const uint8_t *payload, size_t len,
    const char *const *paths, size_t count, plsm_field_value_t *out_values,
    plsm_buf_t *out_bytes, plsm_error_t **out_err);
/* Additive immutable snapshot API. Owners retain four-byte-aligned private
 * bytes independently of pool/stream lifetime. Retain/release and immutable
 * reads may run concurrently on independently held references. Keep an owner
 * alive through every borrowed access; never alter or forge a value_ref.
 * from_lite3 copies caller bytes. View readers return owned snapshots.
 * Value references and borrowed key/string/byte spans need no individual free.
 * Mutation requires copying. A zero owner represents an absent value.
 */
typedef struct plsm_message_view plsm_message_view_t;
typedef plsm_message_view_t plsm_message_t;
typedef enum plsm_feed_input_kind {
    PLSM_FEED_JSON = 1,
    PLSM_FEED_LITE3 = 2,
    PLSM_FEED_MESSAGE = 3,
    PLSM_FEED_BUILDER = 4
} plsm_feed_input_kind_t;
/* JSON input is an object. Lite3 input is a stored envelope. Message input is
 * a direct plsm_message_t pointer with length zero. A builder input is a direct
 * plsm_builder_t pointer with length zero. Leave tags unset when
 * forwarding. The returned message owns its bytes and survives closing the pool.
 * Release it with plsm_message_release. */
int plsm_pool_feed(plsm_pool_t *, uint32_t kind, const void *input, size_t len,
    const plsm_feed_options_t *, const plsm_message_t **, plsm_error_t **);
/* UDP feed reports socket admission and returns no committed message. */
int plsm_udp_pool_feed(plsm_udp_pool_t *, uint32_t kind, const void *input, size_t len,
    const plsm_feed_options_t *, plsm_error_t **);
int plsm_udp_pool_follow(plsm_udp_pool_t *, const plsm_read_options_t *, plsm_udp_follow_t **, plsm_error_t **);
int plsm_udp_pool_fetch(plsm_udp_pool_t *, const plsm_read_options_t *, plsm_udp_follow_t **, plsm_error_t **);

/* Wait for one owned message. Return 1 for an item, 0 for a timeout, 2 for
 * completion, 3 for cancellation, or a negative value for an error. */
int plsm_stream_next_message(plsm_stream_t *, uint64_t timeout_ms,
    const plsm_message_t **, plsm_error_t **);
/* Borrow the sender's UTF-8 bytes while the owner lives. NULL means absent. */
const uint8_t *plsm_message_sender(const plsm_message_t *, size_t *len);
typedef struct plsm_value_ref {
    const plsm_message_view_t *owner;
    size_t offset;
} plsm_value_ref_t;
/* Borrow the tags array and data while the message owner lives. */
plsm_value_ref_t plsm_message_tags(const plsm_message_t *);
#define plsm_message_retain plsm_view_retain
#define plsm_message_release plsm_view_release
#define plsm_message_seq plsm_view_seq
#define plsm_message_time plsm_view_time
#define plsm_message_data plsm_view_data
#define plsm_message_raw plsm_view_bytes
#define plsm_message_to_json plsm_view_to_json
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
#ifdef PLSM_INTERNAL_ABI
/* New view API: exact integer tokens must fit i64; rejects before publication. */
int plsm_pool_feed_json_view(plsm_pool_t *pool, const uint8_t *json, size_t len,
    const plsm_feed_options_t *options, const plsm_message_view_t **out, plsm_error_t **err);
int plsm_pool_feed_view(plsm_pool_t *pool, const plsm_message_view_t *source,
    const plsm_feed_options_t *options, const plsm_message_view_t **out, plsm_error_t **err);
#endif
void plsm_view_retain(const plsm_message_view_t *view);
void plsm_view_release(const plsm_message_view_t *view);
plsm_value_ref_t plsm_view_root(const plsm_message_view_t *view);
plsm_value_ref_t plsm_view_data(const plsm_message_view_t *view);
const uint8_t *plsm_view_bytes(const plsm_message_view_t *view, size_t *len);
/* Conservative whole-owner property: finite numbers, exact binary64 integers,
 * and no native bytes. False for NULL or an owner whose builder did not track it. */
bool plsm_view_json_native_safe(const plsm_message_view_t *view);
uint64_t plsm_view_seq(const plsm_message_view_t *view);
uint64_t plsm_view_time(const plsm_message_view_t *view);
/* Reserved payload storage capacity, excluding owner/index metadata. Shared
 * children/forwarded owners retain the same storage; NULL returns zero. */
size_t plsm_view_retained_bytes(const plsm_message_view_t *view);
int plsm_value_get(plsm_value_ref_t value, const uint8_t *key, size_t len,
    plsm_value_ref_t *out, plsm_error_t **err);
int plsm_value_index(plsm_value_ref_t value, uint32_t index,
    plsm_value_ref_t *out, plsm_error_t **err);
int plsm_value_read(plsm_value_ref_t value, plsm_view_value_t *out, plsm_error_t **err);
/* For a valid container, out_count receives the required capacity. A smaller
 * buffer returns Usage without filling slots. Keys use unique canonical
 * last-entry values. */
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
 * plsm_pool_feed with PLSM_FEED_BUILDER consumes the contents after validating the
 * arguments. Free the handle once afterward. A build error stops publication.
 * A flush error can follow publication, as for other appends. */
typedef struct plsm_builder plsm_builder_t;
#ifdef PLSM_INTERNAL_ABI
/* View reads: plsm_view_stream_next reads immutable message views from a
 * plsm_stream_t opened by plsm_pool_follow or plsm_pool_fetch, as an
 * alternative to plsm_stream_next_wait. The caller owns each returned view.
 * Use plsm_stream_cancel and plsm_stream_free. Serialize reads. Cancel may run
 * concurrently.
 * next waits for the first matching item. Local reads then acquire available
 * companions without waiting. Each HTTPS call returns at most one item.
 * max_count/max_bytes must be positive; bytes bound encoded payload snapshots.
 * An oversized first record errors without consuming it. Each returned owner
 * requires plsm_view_release, including any prefix returned before an error.
 * Zero polls once; UINT64_MAX waits without a time limit.
 * Status: 0=wait expired, 1=items, 2=finished, 3=cancelled, -1=error.
 * out has max_count slots; count always reports initialized owners. */
int plsm_view_stream_next(plsm_stream_t *stream, uint64_t timeout_ms,
    size_t max_count, size_t max_bytes, const plsm_message_view_t **out,
    size_t *count, plsm_error_t **err);
#endif

/* UDP views reuse cancel/close and 1/event, 0/timeout, 2/end, 3/cancelled
 * statuses. Use next_view with view readers. Initialize event to zero
 * and free it normally; payload stays empty. DATA returns an independent owner
 * (release with plsm_view_release); controls return NULL. Other statuses leave
 * outputs untouched. Close only after active calls return. */
int plsm_udp_follow_next_view(plsm_udp_follow_t *follow, uint64_t timeout_ms,
    plsm_udp_event_t *out_event, const plsm_message_view_t **out_owner,
    plsm_error_t **err);

int plsm_builder_new(const plsm_text_t *tags, size_t count,
    plsm_builder_t **out, size_t *data_node, plsm_error_t **err);
int plsm_builder_put(plsm_builder_t *builder, size_t parent,
    const uint8_t *key, size_t key_len, const plsm_view_value_t *value,
    size_t *child_node, plsm_error_t **err);
#ifdef PLSM_INTERNAL_ABI
int plsm_pool_feed_builder_view(plsm_pool_t *pool, plsm_builder_t *builder,
    const plsm_feed_options_t *options, const plsm_message_view_t **out, plsm_error_t **err);
#endif
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
