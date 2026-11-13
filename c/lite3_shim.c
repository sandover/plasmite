/*
Purpose: Implement the C shim ABI that Rust binds to for selected Lite3 APIs.
Exports: `plasmite_lite3_*` symbols declared in `c/lite3_shim.h`.
Role: Thin adapter over the vendored Lite3 library to keep Rust FFI minimal and stable.
Invariants: Callers free JSON text with `plasmite_lite3_free` and parsed documents with `plasmite_lite3_json_doc_free`.
Invariants: Validate reader preconditions before forwarding to Lite3; keep business logic outside this shim.
*/
#include "lite3_shim.h"

#include <errno.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>

#include "lite3.h"
#include "yyjson/yyjson.h"

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

/* Validate UTF-8 without constructing JSON. Skip ASCII in bounded words. */
static int valid_utf8(const char *text, size_t len)
{
        const unsigned char *p = (const unsigned char *)text;
        while (len) {
                if (len >= sizeof(uint64_t)) {
                        uint64_t word;
                        memcpy(&word, p, sizeof(word));
                        if ((word & UINT64_C(0x8080808080808080)) == 0) {
                                p += sizeof(word);
                                len -= sizeof(word);
                                continue;
                        }
                }
                unsigned c = *p++;
                --len;
                if (c < 0x80) continue;
                unsigned count, lo = 0x80, hi = 0xbf;
                if (c >= 0xc2 && c <= 0xdf) count = 1;
                else if (c >= 0xe0 && c <= 0xef) {
                        count = 2;
                        if (c == 0xe0) lo = 0xa0;
                        if (c == 0xed) hi = 0x9f;
                } else if (c >= 0xf0 && c <= 0xf4) {
                        count = 3;
                        if (c == 0xf0) lo = 0x90;
                        if (c == 0xf4) hi = 0x8f;
                } else return 0;
                if (len < count || p[0] < lo || p[0] > hi) return 0;
                for (unsigned i = 1; i < count; ++i) {
                        if (p[i] < 0x80 || p[i] > 0xbf) return 0;
                }
                p += count;
                len -= count;
        }
        return 1;
}

/* The pinned node index stores seven ordered hashes and eight child offsets. */
_Static_assert(LITE3_NODE_SIZE == 96 && LITE3_NODE_SIZE_KC_OFFSET == 32,
               "view validation targets the pinned seven-key index");
typedef struct {
        int (*duplicate)(void *, size_t);
        void *context;
} view_check;
static uint32_t view_word(const unsigned char *buf, size_t ofs)
{
        uint32_t value;
        memcpy(&value, buf + ofs, sizeof value);
        return value;
}
static int verify_view_index_node(const unsigned char *buf, size_t len,
                                  size_t ofs, uint8_t type)
{
        if (verify_node(buf, len, ofs) < 0 || buf[ofs] != type) return -1;
        unsigned count = view_word(buf, ofs + 32) & 7;
        int children = view_word(buf, ofs + 64) != 0;
        for (unsigned i = 1; i <= count; ++i) {
                if ((view_word(buf, ofs + 64 + 4 * i) != 0) != children) return -1;
        }
        return 0;
}
/* Collision-only lookup: no key parsing, allocation, or full-tree walk. */
static int view_has_hash(const unsigned char *buf, size_t len, size_t ofs,
                         uint32_t hash, uint8_t type)
{
        for (unsigned depth = 0; depth <= LITE3_TREE_HEIGHT_MAX; ++depth) {
                if (verify_view_index_node(buf, len, ofs, type) < 0) return -1;
                unsigned count = view_word(buf, ofs + 32) & 7;
                unsigned i = 0;
                while (i < count && view_word(buf, ofs + 4 + 4 * i) < hash) ++i;
                if (i < count && view_word(buf, ofs + 4 + 4 * i) == hash) return 1;
                uint32_t child = view_word(buf, ofs + 64 + 4 * i);
                if (!child) return 0;
                ofs = child;
        }
        return -1;
}

/* The vendored JSON encoder expects terminated keys and string values. */
static int verify_tree(
        const unsigned char *buf, size_t buf_len, size_t ofs, unsigned depth, int *json_safe, view_check *check)
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

        if (check && verify_view_index_node(buf, buf_len, ofs, type) < 0) return -1;
        lite3_iter iter;
        if (lite3_iter_create(buf, buf_len, ofs, &iter) < 0) {
                return -1;
        }
        int ret;
        lite3_str key;
        size_t val_ofs;
        size_t count = 0;
        uint32_t previous_hash = 0;
        int duplicate_reported = 0;
        for (;;) {
                uint32_t stored_hash = 0;
                if (check) {
                        size_t node = iter.node_ofs[iter.depth];
                        unsigned i = iter.node_i[iter.depth];
                        if (i > 7 || verify_view_index_node(buf, buf_len, node, type) < 0) return -1;
                        stored_hash = view_word(buf, node + 4 + 4 * i);
                }
                ret = lite3_iter_next(buf, buf_len, &iter,
                                     type == LITE3_TYPE_OBJECT ? &key : NULL,
                                     &val_ofs);
                if (ret != LITE3_ITER_ITEM) break;
                if (check && ((count && stored_hash <= previous_hash) ||
                              (type == LITE3_TYPE_ARRAY && stored_hash != count))) return -1;
                previous_hash = stored_hash;
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
                if (check && type == LITE3_TYPE_OBJECT) {
                        size_t key_len = val_ofs - (size_t)((const unsigned char *)key.ptr - buf) - 1;
                        if (memchr(key.ptr, 0, key_len) || !valid_utf8(key.ptr, key_len)) return -1;
                        uint32_t base = lite3_get_key_data(key.ptr).hash;
                        if (base != stored_hash) {
                                unsigned attempt;
                                for (attempt = 1; attempt < LITE3_HASH_PROBE_MAX; ++attempt) {
                                        uint32_t prior = base + (attempt - 1) * (attempt - 1);
                                        if (view_has_hash(buf, buf_len, ofs, prior, type) != 1) return -1;
                                        if (base + attempt * attempt == stored_hash) break;
                                }
                                if (attempt == LITE3_HASH_PROBE_MAX) return -1;
                                size_t first;
                                if (plasmite_lite3_get_val_ofs(buf, buf_len, ofs, key.ptr, &first) < 0) return -1;
                                if (first != val_ofs && !duplicate_reported) {
                                        duplicate_reported = 1;
                                        if (check->duplicate(check->context, ofs) < 0) return -1;
                                }
                        }
                }
                ++count;
                if (json_safe && type == LITE3_TYPE_OBJECT &&
                    !valid_utf8(key.ptr, strlen(key.ptr))) {
                        errno = EIO;
                        return -1;
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
                        if (json_safe && !valid_utf8(
                            (const char *)val->val + sizeof(encoded_len), encoded_len - 1)) {
                                errno = EIO;
                                return -1;
                        }
                } else if ((val->type == LITE3_TYPE_OBJECT ||
                            val->type == LITE3_TYPE_ARRAY) &&
                           verify_tree(buf, buf_len, val_ofs, depth + 1, json_safe, check) < 0) {
                        return -1;
                } else if (check && val->type == LITE3_TYPE_BOOL && val->val[0] > 1) {
                        return -1;
                } else if (json_safe && val->type == LITE3_TYPE_F64) {
                        double value;
                        memcpy(&value, val->val, sizeof(value));
                        if (!isfinite(value)) { errno = EIO; return -1; }
                } else if (json_safe && val->type == LITE3_TYPE_BYTES) {
                        /* Keep the existing base64 exporter as the oracle. */
                        *json_safe = 0;
                } else if (json_safe && val->type > LITE3_TYPE_ARRAY) {
                        errno = EINVAL;
                        return -1;
                }
        }
        if (check) {
                uint32_t declared;
                if (lite3_count((unsigned char *)buf, buf_len, ofs, &declared) < 0 || declared != count) return -1;
        }
        return ret < 0 ? -1 : 0;
}
static int verify_json_tree(const unsigned char *buf, size_t len, size_t ofs,
                            unsigned depth, int *json_safe)
{
        return verify_tree(buf, len, ofs, depth, json_safe, NULL);
}
int plasmite_lite3_verify_view(const unsigned char *buf, size_t len,
                              int (*duplicate)(void *, size_t), void *context)
{
        int json_safe = 1;
        view_check check = {duplicate, context};
        return verify_tree(buf, len, 0, 0, &json_safe, &check);
}

/* 0: JSON values; 1: native bytes require base64 formatting; -1: invalid. */
int plasmite_lite3_verify_json(const unsigned char *buf, size_t len, size_t ofs)
{
        int json_safe = 1;
        if (verify_json_tree(buf, len, ofs, 0, &json_safe) < 0) return -1;
        return json_safe ? 0 : 1;
}

int plasmite_lite3_entries(const unsigned char *buf, size_t len, size_t ofs,
                          plasmite_lite3_entry *out, size_t count)
{
        lite3_iter iter;
        if (verify_node(buf, len, ofs) < 0 || lite3_iter_create(buf, len, ofs, &iter) < 0)
                return -1;
        size_t i = 0, offset;
        lite3_str key;
        int object = buf[ofs] == LITE3_TYPE_OBJECT;
        int ret;
        while ((ret = lite3_iter_next(buf, len, &iter, object ? &key : NULL, &offset)) == LITE3_ITER_ITEM) {
                if (i == count) { errno = EINVAL; return -1; }
                out[i++] = (plasmite_lite3_entry){object ? key.ptr : NULL,
                                                object ? strlen(key.ptr) : 0, offset};
        }
        if (ret < 0 || i != count) { errno = EINVAL; return -1; }
        return 0;
}

void plasmite_lite3_value_view(const unsigned char *buf, size_t ofs, plasmite_lite3_value *out)
{
        lite3_val *value = (lite3_val *)(buf + ofs);
        *out = (plasmite_lite3_value){.type = value->type};
        switch (value->type) {
        case LITE3_TYPE_BOOL: out->integer = lite3_val_bool(value); break;
        case LITE3_TYPE_I64: out->integer = lite3_val_i64(value); break;
        case LITE3_TYPE_F64: out->real = lite3_val_f64(value); break;
        case LITE3_TYPE_STRING: out->text = lite3_val_str_n(value, &out->text_len); break;
        case LITE3_TYPE_BYTES:
                out->text = (const char *)lite3_val_bytes(value, &out->text_len); break;
        default: break;
        }
}

/* The pinned decoder's recursive builders borrow the document. Only its
 * top-level _lite3_json_dec_doc frees it. Reuse those same builders while
 * keeping one document alive across independent, zero-initialized attempts.
 */
extern int _lite3_json_dec_obj(unsigned char *, size_t *, size_t, size_t,
                             size_t, yyjson_doc *, yyjson_val *);
extern int _lite3_json_dec_arr(unsigned char *, size_t *, size_t, size_t,
                             size_t, yyjson_doc *, yyjson_val *);

/* Match JSON value semantics before the C-string-keyed storage builder runs.
 * A signed integer zero comes only from the token -0; preserve its sign.
 */
static int prepare_json_values(yyjson_val *value, unsigned depth)
{
        if (depth > LITE3_JSON_NESTING_DEPTH_MAX || yyjson_is_raw(value)) { errno = EINVAL; return -1; }
        if (yyjson_is_sint(value) && yyjson_get_sint(value) == 0)
                yyjson_set_real(value, -0.0);
        if (yyjson_is_obj(value)) {
                yyjson_obj_iter iter = yyjson_obj_iter_with(value);
                yyjson_val *key;
                while ((key = yyjson_obj_iter_next(&iter))) {
                        if (strlen(yyjson_get_str(key)) != yyjson_get_len(key)) {
                                errno = EINVAL; return -1;
                        }
                        if (prepare_json_values(yyjson_obj_iter_get_val(key), depth + 1) < 0) return -1;
                }
        } else if (yyjson_is_arr(value)) {
                yyjson_arr_iter iter = yyjson_arr_iter_with(value);
                yyjson_val *item;
                while ((item = yyjson_arr_iter_next(&iter)))
                        if (prepare_json_values(item, depth + 1) < 0) return -1;
        }
        return 0;
}

static void *json_parse(const char *json, size_t len, size_t *values, yyjson_read_flag flags)
{
        errno = 0;
        yyjson_doc *doc = yyjson_read_opts((char *)json, len, flags, NULL, NULL);
        if (!doc) errno = EINVAL;
        else if (prepare_json_values(yyjson_doc_get_root(doc), 0) < 0) {
                yyjson_doc_free(doc); doc = NULL;
        } else *values = yyjson_doc_get_val_count(doc);
        return doc;
}

void *plasmite_lite3_json_parse(const char *json, size_t len, size_t *values)
{ return json_parse(json, len, values, YYJSON_READ_NOFLAG); }
void *plasmite_lite3_json_parse_view(const char *json, size_t len, size_t *values)
{ return json_parse(json, len, values, YYJSON_READ_BIGNUM_AS_RAW); }

uintptr_t plasmite_json_root(void *doc) { return (uintptr_t)yyjson_doc_get_root(doc); }
size_t plasmite_json_count(uintptr_t node) { return yyjson_get_len((yyjson_val *)node); }
void plasmite_json_value_view(uintptr_t node, plasmite_lite3_value *out)
{
        yyjson_val *value = (yyjson_val *)node;
        *out = (plasmite_lite3_value){0};
        switch (yyjson_get_type(value)) {
        case YYJSON_TYPE_NULL: out->type = LITE3_TYPE_NULL; break;
        case YYJSON_TYPE_BOOL: out->type = LITE3_TYPE_BOOL; out->integer = yyjson_get_bool(value); break;
        case YYJSON_TYPE_STR:
                out->type = LITE3_TYPE_STRING; out->text = yyjson_get_str(value); out->text_len = yyjson_get_len(value); break;
        case YYJSON_TYPE_OBJ: out->type = LITE3_TYPE_OBJECT; break;
        case YYJSON_TYPE_ARR: out->type = LITE3_TYPE_ARRAY; break;
        case YYJSON_TYPE_NUM:
                if (yyjson_is_real(value)) { out->type = LITE3_TYPE_F64; out->real = yyjson_get_real(value); }
                else if (yyjson_is_uint(value)) {
                        out->type = 9; /* Parsed JSON uint64; Lite3 stores only signed integers. */
                        uint64_t integer = yyjson_get_uint(value);
                        memcpy(&out->integer, &integer, sizeof(integer));
                } else { out->type = LITE3_TYPE_I64; out->integer = yyjson_get_sint(value); }
                break;
        default: out->type = LITE3_TYPE_INVALID; break;
        }
}
int plasmite_json_entries(uintptr_t node, plasmite_lite3_entry *out, size_t count)
{
        yyjson_val *value = (yyjson_val *)node;
        size_t i = 0;
        if (yyjson_is_obj(value)) {
                yyjson_obj_iter iter = yyjson_obj_iter_with(value);
                yyjson_val *key;
                while ((key = yyjson_obj_iter_next(&iter))) {
                        if (i == count) return -1;
                        out[i++] = (plasmite_lite3_entry){yyjson_get_str(key), yyjson_get_len(key),
                                                        (uintptr_t)yyjson_obj_iter_get_val(key)};
                }
        } else {
                yyjson_arr_iter iter = yyjson_arr_iter_with(value);
                yyjson_val *item;
                while ((item = yyjson_arr_iter_next(&iter))) {
                        if (i == count) return -1;
                        out[i++] = (plasmite_lite3_entry){NULL, 0, (uintptr_t)item};
                }
        }
        return i == count ? 0 : -1;
}

int plasmite_json_message_build(void *parsed, const plasmite_text_ref *tags, size_t count,
                               unsigned char *buf, size_t *out_len, size_t capacity)
{
        yyjson_doc *doc = parsed;
        yyjson_val *root = yyjson_doc_get_root(doc);
        if (!yyjson_is_obj(root)) { errno = EINVAL; return -1; }
        size_t meta, array, data;
        if (lite3_init_obj(buf, out_len, capacity) < 0 ||
            lite3_set_obj(buf, out_len, 0, capacity, "meta", &meta) < 0 ||
            lite3_set_arr(buf, out_len, meta, capacity, "tags", &array) < 0) return -1;
        for (size_t i = 0; i < count; ++i)
                if (lite3_arr_append_str_n(buf, out_len, array, capacity, tags[i].ptr, tags[i].len) < 0) return -1;
        if (lite3_set_obj(buf, out_len, 0, capacity, "data", &data) < 0) return -1;
        return _lite3_json_dec_obj(buf, out_len, data, capacity, 1, doc, root);
}

void plasmite_lite3_json_doc_free(void *doc)
{
        yyjson_doc_free((yyjson_doc *)doc);
}

int plasmite_lite3_json_build(void *parsed, unsigned char *buf, size_t *out_len, size_t buf_sz)
{
        errno = 0;
        yyjson_doc *doc = (yyjson_doc *)parsed;
        yyjson_val *root = yyjson_doc_get_root(doc);
        int ret;
        switch (yyjson_get_type(root)) {
        case YYJSON_TYPE_OBJ:
                ret = lite3_init_obj(buf, out_len, buf_sz);
                if (ret < 0) return ret;
                return _lite3_json_dec_obj(buf, out_len, 0, buf_sz, 0, doc, root);
        case YYJSON_TYPE_ARR:
                ret = lite3_init_arr(buf, out_len, buf_sz);
                if (ret < 0) return ret;
                return _lite3_json_dec_arr(buf, out_len, 0, buf_sz, 0, doc, root);
        default:
                /* Preserve the pinned decoder's empty-output scalar behavior.
                 * Canonical message validation separately requires an object.
                 */
                errno = EINVAL;
                return 0;
        }
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
        if (verify_json_tree(buf, buf_len, ofs, 0, NULL) < 0) {
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
        if (verify_json_tree(buf, buf_len, ofs, 0, NULL) < 0) {
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

int plasmite_lite3_arr_get_val_ofs(const unsigned char *buf, size_t len, size_t ofs, uint32_t index, size_t *out)
{
        if (verify_node(buf, len, ofs) < 0) return -1;
        lite3_val *value;
        if (_lite3_get_by_index(buf, len, ofs, index, &value) < 0) return -1;
        *out = (size_t)((const unsigned char *)value - buf);
        return 0;
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

/* Retry only the insertion after ENOBUFS; the caller owns growth and value data. */
int plasmite_lite3_put(unsigned char *buf, size_t *len, size_t capacity,
                      size_t parent, const char *key,
                      const plasmite_lite3_value *value, size_t *child)
{
        if (parent == SIZE_MAX) return lite3_init_obj(buf, len, capacity);
        if (key) {
                switch (value->type) {
                case LITE3_TYPE_NULL: return lite3_set_null(buf, len, parent, capacity, key);
                case LITE3_TYPE_BOOL: return lite3_set_bool(buf, len, parent, capacity, key, value->integer != 0);
                case LITE3_TYPE_I64: return lite3_set_i64(buf, len, parent, capacity, key, value->integer);
                case LITE3_TYPE_F64: return lite3_set_f64(buf, len, parent, capacity, key, value->real);
                case LITE3_TYPE_STRING: return lite3_set_str_n(buf, len, parent, capacity, key, value->text, value->text_len);
                case LITE3_TYPE_BYTES: return lite3_set_bytes(buf, len, parent, capacity, key, (const unsigned char *)value->text, value->text_len);
                case LITE3_TYPE_OBJECT: return lite3_set_obj(buf, len, parent, capacity, key, child);
                case LITE3_TYPE_ARRAY: return lite3_set_arr(buf, len, parent, capacity, key, child);
                }
        } else {
                switch (value->type) {
                case LITE3_TYPE_NULL: return lite3_arr_append_null(buf, len, parent, capacity);
                case LITE3_TYPE_BOOL: return lite3_arr_append_bool(buf, len, parent, capacity, value->integer != 0);
                case LITE3_TYPE_I64: return lite3_arr_append_i64(buf, len, parent, capacity, value->integer);
                case LITE3_TYPE_F64: return lite3_arr_append_f64(buf, len, parent, capacity, value->real);
                case LITE3_TYPE_STRING: return lite3_arr_append_str_n(buf, len, parent, capacity, value->text, value->text_len);
                case LITE3_TYPE_BYTES: return lite3_arr_append_bytes(buf, len, parent, capacity, (const unsigned char *)value->text, value->text_len);
                case LITE3_TYPE_OBJECT: return lite3_arr_append_obj(buf, len, parent, capacity, child);
                case LITE3_TYPE_ARRAY: return lite3_arr_append_arr(buf, len, parent, capacity, child);
                }
        }
        errno = EINVAL;
        return -1;
}

/* Private inline cursor state. It retains offsets only, never caller buffers. */
typedef struct { lite3_iter iter; int object; } plasmite_cursor;
_Static_assert(LITE3_NODE_SIZE == 96 && LITE3_NODE_SIZE_KC_OFFSET == 32, "cursor targets pinned seven-key node layout");
_Static_assert(sizeof(plasmite_cursor) <= 256, "private cursor storage too small");
_Static_assert(_Alignof(plasmite_cursor) <= _Alignof(uint64_t), "private cursor alignment");
int plasmite_lite3_cursor_init(const unsigned char *buf, size_t len, size_t ofs,
                              void *state, size_t size)
{
        if (!state || size < sizeof(plasmite_cursor) ||
            (uintptr_t)state % _Alignof(plasmite_cursor) || verify_node(buf, len, ofs) < 0)
                return -1;
        plasmite_cursor *cursor = state;
        cursor->object = buf[ofs] == LITE3_TYPE_OBJECT;
        return lite3_iter_create(buf, len, ofs, &cursor->iter);
}
int plasmite_lite3_cursor_next(const unsigned char *buf, size_t len, void *state,
                              plasmite_lite3_entry *out, uint32_t *hash)
{
        plasmite_cursor *cursor = state;
        /* The pinned node layout begins with gen_type followed by hashes[7]. */
        size_t node = cursor->iter.node_ofs[cursor->iter.depth];
        uint8_t index = cursor->iter.node_i[cursor->iter.depth];
        uint32_t stored_hash;
        if (verify_node(buf, len, node) < 0 || index > 7) return -1;
        memcpy(&stored_hash, buf + node + sizeof(uint32_t) * (1 + index), sizeof stored_hash);
        lite3_str key;
        size_t offset;
        int result = lite3_iter_next(buf, len, &cursor->iter,
                                    cursor->object ? &key : NULL, &offset);
        if (result == LITE3_ITER_ITEM) {
                size_t key_len = 0;
                if (cursor->object) {
                        size_t key_ofs = (size_t)((const unsigned char *)key.ptr - buf);
                        if (offset <= key_ofs) return -1;
                        /* The pinned iterator's key.len omits its tag shift.
                         * Its checked value offset gives the exact key span. */
                        key_len = offset - key_ofs - 1;
                        if (verify_string(buf, len, key.ptr, key_len) < 0) return -1;
                }
                *out = (plasmite_lite3_entry){cursor->object ? key.ptr : NULL,
                                             key_len, offset};
                if (hash) *hash = stored_hash;
        }
        return result;
}

/* Rust/host strings already have a length; terminate short keys on the stack
 * instead of allocating another string at each language boundary. */
int plasmite_lite3_lookup_span(const unsigned char *buf, size_t len, size_t ofs,
                              const char *key, size_t key_len, size_t *out)
{
        char local[256];
        if (key_len == SIZE_MAX) { errno = EINVAL; return -1; }
        char *terminated = key_len < sizeof local ? local : malloc(key_len + 1);
        if (!terminated) { errno = ENOMEM; return -1; }
        memcpy(terminated, key, key_len);
        terminated[key_len] = '\0';
        int result = plasmite_lite3_get_val_ofs(buf, len, ofs, terminated, out);
        int error = errno;
        if (terminated != local) free(terminated);
        errno = error;
        return result;
}
