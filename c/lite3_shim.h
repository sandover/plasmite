// C shim interface for selected Lite3 APIs used by Rust.
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

void plasmite_lite3_free(void *ptr);

#ifdef __cplusplus
}
#endif

#endif
