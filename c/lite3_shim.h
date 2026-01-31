/*
Purpose: Expose a small, stable C ABI for Lite3 functionality used by Rust.
Exports: `plasmite_lite3_json_dec`, `plasmite_lite3_json_enc(_pretty)`, `plasmite_lite3_get_*`, `plasmite_lite3_free`.
Role: Thin boundary between the Rust crate and the vendored Lite3 implementation.
Invariants: Function signatures are part of the Rust FFI contract; change with care.
Invariants: Returned heap pointers are freed by calling `plasmite_lite3_free`.
*/
#ifndef PLASMITE_LITE3_SHIM_H
#define PLASMITE_LITE3_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

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

int plasmite_lite3_count(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        uint32_t *out);

int plasmite_lite3_arr_get_type(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        uint32_t index,
        uint8_t *out_type);

void plasmite_lite3_free(void *ptr);

#ifdef __cplusplus
}
#endif

#endif
