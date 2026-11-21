#ifndef PLASMITE_GO_BORROWED_H
#define PLASMITE_GO_BORROWED_H
/* Binding-private synchronous reader. Only views.go may supply proof offsets.
 * The input is an unexported immutable Go allocation validated once. C retains
 * no pointers. Every pointer-free slot refers to that same allocation. */
#define PLSM_INTERNAL_ABI
#include "plasmite.h"
typedef struct {
    uint32_t kind, reserved;
    int64_t integer;
    double real;
    size_t node, offset, len;
} plsm_go_value;
int plsm_go_validate(const uint8_t *, size_t, size_t *, size_t *, plsm_buf_t *, plsm_error_t **);
int plsm_go_select(const uint8_t *, size_t, const size_t *, size_t, size_t,
    const plsm_selector_t *, plsm_go_value *, size_t, plsm_error_t **);
int plsm_go_json(const uint8_t *, size_t, size_t, plsm_buf_t *, plsm_error_t **);

int plsm_go_message_json(const uint8_t *, size_t, const size_t *, size_t, size_t, uint64_t, uint64_t, plsm_buf_t *, plsm_error_t **);

/* Return small C results by value. Go need not heap-allocate one escaping output
 * variable per pointer argument, and every result pointer owns native memory. */
typedef struct { int status; plsm_error_t *error; size_t data,meta; plsm_buf_t proof; } plsm_go_validation_t;
static inline plsm_go_validation_t plsm_go_validate_result(const uint8_t *data,size_t len) {
    plsm_go_validation_t result = {0};
    result.status = plsm_go_validate(data,len,&result.data,&result.meta,&result.proof,&result.error);
    return result;
}
typedef struct { int status; plsm_error_t *error; } plsm_go_status_t;
static inline plsm_go_status_t plsm_go_select_result(const uint8_t *data,size_t len,const size_t *duplicates,size_t count,size_t root,const plsm_selector_t *selector,plsm_go_value *values,size_t capacity) {
    plsm_go_status_t result = {0};
    result.status = plsm_go_select(data,len,duplicates,count,root,selector,values,capacity,&result.error);
    return result;
}
typedef struct { int status; plsm_error_t *error; plsm_buf_t buffer; } plsm_go_json_result_t;
static inline plsm_go_json_result_t plsm_go_json_result(const uint8_t *data,size_t len,size_t root) {
    plsm_go_json_result_t result = {0};
    result.status = plsm_go_json(data,len,root,&result.buffer,&result.error);
    return result;
}
static inline plsm_go_json_result_t plsm_go_message_result(const uint8_t *data,size_t len,const size_t *duplicates,size_t count,size_t root,uint64_t seq,uint64_t timestamp) {
    plsm_go_json_result_t result = {0};
    result.status = plsm_go_message_json(data,len,duplicates,count,root,seq,timestamp,&result.buffer,&result.error);
    return result;
}
static inline void plsm_go_free_buffer(plsm_buf_t buffer) { plsm_buf_free(&buffer); }

typedef struct plsm_go_decoder plsm_go_decoder_t;
int plsm_go_decoder_new(const plsm_selector_t *,const uint8_t *,size_t,plsm_go_decoder_t **,plsm_error_t **);
void plsm_go_decoder_free(plsm_go_decoder_t *);
int plsm_go_decode_fields(const uint8_t *,size_t,const size_t *,size_t,size_t,const plsm_go_decoder_t *,plsm_go_value *,size_t,plsm_error_t **);
typedef struct { int status; plsm_error_t *error; plsm_go_decoder_t *decoder; } plsm_go_decoder_result_t;
static inline plsm_go_decoder_result_t plsm_go_prepare_decoder(const plsm_selector_t *selector,const uint8_t *objects,size_t count) {
    plsm_go_decoder_result_t result={0};
    result.status=plsm_go_decoder_new(selector,objects,count,&result.decoder,&result.error);
    return result;
}
static inline plsm_go_status_t plsm_go_decode_result(const uint8_t *data,size_t len,const size_t *duplicates,size_t count,size_t root,const plsm_go_decoder_t *decoder,plsm_go_value *values,size_t capacity) {
    plsm_go_status_t result={0};
    result.status=plsm_go_decode_fields(data,len,duplicates,count,root,decoder,values,capacity,&result.error);
    return result;
}

/* Pinned input data borrows Go strings/immutable buffers for one call only.
 * Mutable bytes and hook JSON are private snapshots. No pointer escapes.
 * Kinds 11/12/13 carry packed native-endian i64/f64/u8-bool array snapshots
 * in text; these internal spans never become a second persisted encoding. */
typedef struct { const uint8_t *data; size_t len; } plsm_go_input_span_t;
typedef struct {
    size_t parent;
    uint32_t kind, flags;
    int64_t integer;
    double real;
    plsm_go_input_span_t key, text;
} plsm_go_input_node_t;

int plsm_go_append_nodes(plsm_pool_t *,const plsm_go_input_node_t *,size_t,const plsm_go_input_span_t *,size_t,uint32_t,const plsm_message_view_t **,plsm_error_t **);
typedef struct {
    int status;
    plsm_error_t *error;
    const plsm_message_view_t *owner;
    const uint8_t *bytes;
    size_t len,data,meta;
    uint64_t seq,timestamp;
} plsm_go_input_result_t;
static inline plsm_go_input_result_t plsm_go_append_input(plsm_pool_t *pool,const plsm_go_input_node_t *nodes,size_t count,const plsm_go_input_span_t *tags,size_t tag_count,uint32_t durability) {
    plsm_go_input_result_t result={0};
    result.status=plsm_go_append_nodes(pool,nodes,count,tags,tag_count,durability,&result.owner,&result.error);
    if (result.status) return result;
    result.bytes=plsm_view_bytes(result.owner,&result.len);
    result.seq=plsm_view_seq(result.owner);result.timestamp=plsm_view_time(result.owner);
    result.data=plsm_view_data(result.owner).offset;
    plsm_value_ref_t meta={0};
    result.status=plsm_value_get(plsm_view_root(result.owner),(const uint8_t *)"meta",4,&meta,&result.error);
    if (result.status) { plsm_view_release(result.owner);result.owner=NULL;return result; }
    result.meta=meta.offset;
    return result;
}


/* Return-by-value adapters keep pointer-free output temporaries off Go's heap. */
typedef struct { int status; plsm_error_t *error; const plsm_message_t *owner; } plsm_go_view_next_t;
static inline plsm_go_view_next_t plsm_go_next_view(plsm_stream_t *stream,uint64_t wait) {
    plsm_go_view_next_t result={0};
    result.status=plsm_stream_next_message(stream,wait,&result.owner,&result.error);
    return result;
}
int plsm_go_append_sealed(plsm_pool_t *,const uint8_t *,size_t,uint32_t,uint64_t *,uint64_t *,plsm_error_t **);
int plsm_go_append_checked_view(plsm_pool_t *,const uint8_t *,size_t,size_t,uint32_t,uint64_t *,uint64_t *,plsm_error_t **);
typedef struct { int status; plsm_error_t *error; uint64_t seq,timestamp; } plsm_go_sealed_result_t;
static inline plsm_go_sealed_result_t plsm_go_forward_view(plsm_pool_t *pool,const uint8_t *data,size_t len,uint32_t durability) {
    plsm_go_sealed_result_t result={0};
    result.status=plsm_go_append_sealed(pool,data,len,durability,&result.seq,&result.timestamp,&result.error);
    return result;
}
static inline plsm_go_sealed_result_t plsm_go_forward_checked_view(plsm_pool_t *pool,const uint8_t *data,size_t len,size_t meta,uint32_t durability) {
    plsm_go_sealed_result_t result={0};
    result.status=plsm_go_append_checked_view(pool,data,len,meta,durability,&result.seq,&result.timestamp,&result.error);
    return result;
}

#endif
