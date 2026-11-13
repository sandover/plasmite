/*
Purpose: Expose a small, stable C ABI for Lite3 functionality used by Rust.
Exports: `plasmite_lite3_json_dec`, `plasmite_lite3_json_enc(_pretty)`, `plasmite_lite3_get_*`, `plasmite_lite3_free`.
Role: Thin boundary between the Rust crate and the vendored Lite3 implementation.
Invariants: Function signatures are part of the Rust FFI contract; change with care.
Invariants: Free JSON text with `plasmite_lite3_free`; free parsed documents with `plasmite_lite3_json_doc_free`.
*/
#ifndef PLASMITE_LITE3_SHIM_H
#define PLASMITE_LITE3_SHIM_H

#include <stddef.h>
#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Check JSON readability without export; 1 reports native bytes needing base64. */
int plasmite_lite3_verify_json(const unsigned char *buf, size_t len, size_t ofs);

/* Private borrowed views. First verify the immutable tree with verify_json >= 0;
 * offsets must come from that root or its entries. Pointers borrow buf.
 */
typedef struct {
        const char *key;
        size_t key_len;
        size_t offset;
} plasmite_lite3_entry;
typedef struct {
        uint8_t type;
        int64_t integer;
        double real;
        const char *text;
        size_t text_len;
} plasmite_lite3_value;
int plasmite_lite3_put(unsigned char *buf, size_t *len, size_t capacity,
                      size_t parent, const char *key,
                      const plasmite_lite3_value *value, size_t *child);
int plasmite_lite3_entries(const unsigned char *buf, size_t len, size_t ofs,
                         plasmite_lite3_entry *out, size_t count);
void plasmite_lite3_value_view(const unsigned char *buf, size_t ofs, plasmite_lite3_value *out);

/* The parsed document owns its allocations and never mutates/retains input.
 * Build borrows it; free it once after all output-capacity attempts finish.
 */
void *plasmite_lite3_json_parse(const char *json, size_t len, size_t *values);
void *plasmite_lite3_json_parse_view(const char *json, size_t len, size_t *values);
void plasmite_lite3_json_doc_free(void *doc);
int plasmite_lite3_json_build(void *doc, unsigned char *buf, size_t *out_len, size_t buf_sz);
typedef struct { const char *ptr; size_t len; } plasmite_text_ref;
uintptr_t plasmite_json_root(void *doc);
void plasmite_json_value_view(uintptr_t value, plasmite_lite3_value *out);
size_t plasmite_json_count(uintptr_t value);
int plasmite_json_entries(uintptr_t value, plasmite_lite3_entry *out, size_t count);
int plasmite_json_message_build(void *doc, const plasmite_text_ref *tags, size_t count,
                               unsigned char *buf, size_t *out_len, size_t capacity);

int plasmite_lite3_json_dec(
        const char *json_str,
        size_t json_len,
        unsigned char *buf,
        size_t *out_len,
        size_t buf_sz);

char *plasmite_lite3_json_enc(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        size_t *out_len);

char *plasmite_lite3_json_enc_pretty(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        size_t *out_len);

uint8_t plasmite_lite3_get_root_type(
        const unsigned char *buf,
        size_t buf_len);

uint8_t plasmite_lite3_get_type(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        const char *key);

int plasmite_lite3_get_val_ofs(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        const char *key,
        size_t *out_ofs);

int plasmite_lite3_get_bool(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        const char *key,
        bool *out);

int plasmite_lite3_get_i64(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        const char *key,
        int64_t *out);

int plasmite_lite3_count(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        uint32_t *out);

int plasmite_lite3_arr_get_val_ofs(const unsigned char *buf, size_t len, size_t ofs, uint32_t index, size_t *out);

int plasmite_lite3_arr_get_type(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        uint32_t index,
        uint8_t *out_type);

/* out_ptr borrows bytes from buf; keep buf alive and do not free out_ptr. */
int plasmite_lite3_arr_get_str(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        uint32_t index,
        const char **out_ptr,
        size_t *out_len);

int plasmite_lite3_last_errno(void);

void plasmite_lite3_free(void *ptr);

/* Private fixed-capacity iterator storage, aligned to uint64_t. */
int plasmite_lite3_cursor_init(const unsigned char *buf, size_t len, size_t ofs,
                              void *state, size_t size);
int plasmite_lite3_cursor_next(const unsigned char *buf, size_t len, void *state,
                              plasmite_lite3_entry *out, uint32_t *hash);

int plasmite_lite3_lookup_span(const unsigned char *buf, size_t len, size_t ofs,
                              const char *key, size_t key_len, size_t *out);

int plasmite_lite3_verify_view(const unsigned char *buf, size_t len,
                              int (*duplicate)(void *, size_t), void *context,
                              int *native_json_safe);

#ifdef __cplusplus
}
#endif

#endif

