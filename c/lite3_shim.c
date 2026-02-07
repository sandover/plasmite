/*
Purpose: Implement the C shim ABI that Rust binds to for selected Lite3 APIs.
Exports: `plasmite_lite3_*` symbols declared in `c/lite3_shim.h`.
Role: Thin adapter over the vendored Lite3 library to keep Rust FFI minimal and stable.
Invariants: Returned heap pointers are owned by the caller and freed via `plasmite_lite3_free`.
Invariants: This file should not contain business logic; it only forwards to Lite3.
*/
#include "lite3_shim.h"

#include <stdlib.h>

#include "lite3.h"

int plasmite_lite3_json_dec(
        const char *json_str,
        size_t json_len,
        unsigned char *buf,
        size_t *out_len,
        size_t buf_sz)
{
        return lite3_json_dec(buf, out_len, buf_sz, json_str, json_len);
}

char *plasmite_lite3_json_enc(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        size_t *out_len)
{
        return lite3_json_enc(buf, buf_len, ofs, out_len);
}

char *plasmite_lite3_json_enc_pretty(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        size_t *out_len)
{
        return lite3_json_enc_pretty(buf, buf_len, ofs, out_len);
}

uint8_t plasmite_lite3_get_root_type(const unsigned char *buf, size_t buf_len)
{
        return (uint8_t)lite3_get_root_type(buf, buf_len);
}

uint8_t plasmite_lite3_get_type(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        const char *key)
{
        return (uint8_t)lite3_get_type(buf, buf_len, ofs, key);
}

int plasmite_lite3_get_val_ofs(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        const char *key,
        size_t *out_ofs)
{
        lite3_val *val = NULL;
        lite3_key_data key_data = lite3_get_key_data(key);
        int ret = lite3_get_impl(buf, buf_len, ofs, key, key_data, &val);
        if (ret < 0) {
                return ret;
        }
        if (out_ofs) {
                *out_ofs = (size_t)((const unsigned char *)val - buf);
        }
        return 0;
}

int plasmite_lite3_count(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        uint32_t *out)
{
        return lite3_count((unsigned char *)buf, buf_len, ofs, out);
}

int plasmite_lite3_arr_get_type(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        uint32_t index,
        uint8_t *out_type)
{
        enum lite3_type type = lite3_arr_get_type(buf, buf_len, ofs, index);
        if (type == LITE3_TYPE_INVALID) {
                return -1;
        }
        if (out_type) {
                *out_type = (uint8_t)type;
        }
        return 0;
}

void plasmite_lite3_free(void *ptr)
{
        free(ptr);
}

/*
Build vendored Lite3 implementation units directly into this translation unit.
This avoids archive extraction-order differences across linkers when producing
Rust rlibs that are later linked by multiple binaries in this crate.
*/
#include "../vendor/lite3/src/lite3.c"
#include "../vendor/lite3/src/json_dec.c"
#include "../vendor/lite3/src/json_enc.c"
#include "../vendor/lite3/src/ctx_api.c"
#include "../vendor/lite3/src/debug.c"
#include "../vendor/lite3/lib/yyjson/yyjson.c"
#include "../vendor/lite3/lib/nibble_base64/base64.c"
