#ifndef PLASMITE_GO_BORROWED_H
#define PLASMITE_GO_BORROWED_H
/* Binding-private synchronous reader. Only views.go may supply proof offsets.
 * The input is an unexported immutable Go allocation validated once. C retains
 * no pointers. Every pointer-free slot refers to that same allocation. */
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
typedef struct { int status; plsm_error_t *error; size_t len; uint64_t timestamp; uint32_t flags; } plsm_go_get_result_t;
static inline plsm_go_get_result_t plsm_go_get_into(plsm_pool_t *pool, uint64_t seq, uint8_t *data, size_t capacity) {
    plsm_go_get_result_t result = {0};
    result.status = plsm_pool_get_into(pool,seq,data,capacity,&result.len,&result.timestamp,&result.flags,&result.error);
    return result;
}
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

#endif
