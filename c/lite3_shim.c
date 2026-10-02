/*
Purpose: Implement the C shim ABI that Rust binds to for selected Lite3 APIs.
Exports: `plasmite_lite3_*` symbols declared in `c/lite3_shim.h`.
Role: Thin adapter over the vendored Lite3 library to keep Rust FFI minimal and stable.
Invariants: Returned heap pointers are owned by the caller and freed via `plasmite_lite3_free`.
Invariants: Validate reader preconditions before forwarding to Lite3; keep business logic outside this shim.
*/
#include "lite3_shim.h"

#include <errno.h>
#include <stdlib.h>
#include <string.h>

#include "lite3.h"

/* Lite3's readers cast nodes to aligned structs after their size check. */
static int verify_node(const unsigned char *buf, size_t buf_len, size_t ofs)
{
        if (_lite3_verify_get(buf, buf_len, ofs) < 0) {
                return -1;
        }
        if (((uintptr_t)(buf + ofs) & LITE3_NODE_ALIGNMENT_MASK) != 0) {
                errno = EINVAL;
                return -1;
        }
        return 0;
}

static int verify_string(
        const unsigned char *buf, size_t buf_len, const char *str, size_t len)
{
        uintptr_t start = (uintptr_t)buf;
        uintptr_t address = (uintptr_t)str;
        if (address < start || address - start >= buf_len ||
            len >= buf_len - (address - start) || str[len] != '\0') {
                errno = EINVAL;
                return -1;
        }
        return 0;
}

/* The vendored JSON encoder expects terminated keys and string values. */
static int verify_json_tree(
        const unsigned char *buf, size_t buf_len, size_t ofs, unsigned depth)
{
        if (depth >= LITE3_JSON_NESTING_DEPTH_MAX ||
            verify_node(buf, buf_len, ofs) < 0) {
                errno = EINVAL;
                return -1;
        }
        uint8_t type = buf[ofs];
        if (type != LITE3_TYPE_OBJECT && type != LITE3_TYPE_ARRAY) {
                errno = EINVAL;
                return -1;
        }

        lite3_iter iter;
        if (lite3_iter_create(buf, buf_len, ofs, &iter) < 0) {
                return -1;
        }
        int ret;
        lite3_str key;
        size_t val_ofs;
        while ((ret = lite3_iter_next(buf, buf_len, &iter,
                                     type == LITE3_TYPE_OBJECT ? &key : NULL,
                                     &val_ofs)) == LITE3_ITER_ITEM) {
                if (type == LITE3_TYPE_OBJECT) {
                        uintptr_t address = (uintptr_t)key.ptr;
                        uintptr_t start = (uintptr_t)buf;
                        if (address < start || address - start >= val_ofs ||
                            verify_string(buf, buf_len, key.ptr,
                                          val_ofs - (address - start) - 1) < 0) {
                                errno = EINVAL;
                                return -1;
                        }
                }
                const lite3_val *val = (const lite3_val *)(buf + val_ofs);
                if (val->type == LITE3_TYPE_STRING) {
                        uint32_t encoded_len;
                        memcpy(&encoded_len, val->val, sizeof(encoded_len));
                        if (encoded_len == 0 ||
                            verify_string(buf, buf_len,
                                          (const char *)val->val + sizeof(encoded_len),
                                          encoded_len - 1) < 0) {
                                errno = EINVAL;
                                return -1;
                        }
                } else if ((val->type == LITE3_TYPE_OBJECT ||
                            val->type == LITE3_TYPE_ARRAY) &&
                           verify_json_tree(buf, buf_len, val_ofs, depth + 1) < 0) {
                        return -1;
                }
        }
        return ret < 0 ? -1 : 0;
}

int plasmite_lite3_json_dec(
        const char *json_str,
        size_t json_len,
        unsigned char *buf,
        size_t *out_len,
        size_t buf_sz)
{
        errno = 0;
        return lite3_json_dec(buf, out_len, buf_sz, json_str, json_len);
}

char *plasmite_lite3_json_enc(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        size_t *out_len)
{
        if (verify_json_tree(buf, buf_len, ofs, 0) < 0) {
                return NULL;
        }
        return lite3_json_enc(buf, buf_len, ofs, out_len);
}

char *plasmite_lite3_json_enc_pretty(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        size_t *out_len)
{
        if (verify_json_tree(buf, buf_len, ofs, 0) < 0) {
                return NULL;
        }
        return lite3_json_enc_pretty(buf, buf_len, ofs, out_len);
}

uint8_t plasmite_lite3_get_root_type(const unsigned char *buf, size_t buf_len)
{
        if (verify_node(buf, buf_len, 0) < 0) {
                return LITE3_TYPE_INVALID;
        }
        return (uint8_t)lite3_get_root_type(buf, buf_len);
}

uint8_t plasmite_lite3_get_type(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        const char *key)
{
        if (verify_node(buf, buf_len, ofs) < 0) {
                return LITE3_TYPE_INVALID;
        }
        return (uint8_t)lite3_get_type(buf, buf_len, ofs, key);
}

int plasmite_lite3_get_val_ofs(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        const char *key,
        size_t *out_ofs)
{
        if (verify_node(buf, buf_len, ofs) < 0 ||
            _lite3_verify_obj_get(buf, buf_len, ofs, key) < 0) {
                return -1;
        }
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

int plasmite_lite3_get_bool(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        const char *key,
        bool *out)
{
        if (verify_node(buf, buf_len, ofs) < 0) {
                return -1;
        }
        return lite3_get_bool(buf, buf_len, ofs, key, out);
}

int plasmite_lite3_get_i64(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        const char *key,
        int64_t *out)
{
        if (verify_node(buf, buf_len, ofs) < 0) {
                return -1;
        }
        return lite3_get_i64(buf, buf_len, ofs, key, out);
}

int plasmite_lite3_count(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        uint32_t *out)
{
        if (verify_node(buf, buf_len, ofs) < 0) {
                return -1;
        }
        return lite3_count((unsigned char *)buf, buf_len, ofs, out);
}

int plasmite_lite3_arr_get_type(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        uint32_t index,
        uint8_t *out_type)
{
        if (verify_node(buf, buf_len, ofs) < 0) {
                return -1;
        }
        enum lite3_type type = lite3_arr_get_type(buf, buf_len, ofs, index);
        if (type == LITE3_TYPE_INVALID) {
                return -1;
        }
        if (out_type) {
                *out_type = (uint8_t)type;
        }
        return 0;
}

int plasmite_lite3_arr_get_str(
        const unsigned char *buf,
        size_t buf_len,
        size_t ofs,
        uint32_t index,
        const char **out_ptr,
        size_t *out_len)
{
        if (verify_node(buf, buf_len, ofs) < 0) {
                return -1;
        }
        lite3_str value = {0};
        int ret = lite3_arr_get_str(buf, buf_len, ofs, index, &value);
        if (ret < 0) {
                return ret;
        }
        const char *ptr = LITE3_STR(buf, value);
        if (!ptr) {
                return -1;
        }
        if (verify_string(buf, buf_len, ptr, value.len) < 0) {
                return -1;
        }
        if (out_ptr) {
                *out_ptr = ptr;
        }
        if (out_len) {
                *out_len = (size_t)value.len;
        }
        return 0;
}

int plasmite_lite3_last_errno(void)
{
        return errno;
}

void plasmite_lite3_free(void *ptr)
{
        free(ptr);
}
