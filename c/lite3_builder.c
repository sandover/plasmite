/* Walk an already parsed document in C; cross into Rust only for buffer growth. */
#include "lite3_shim.h"
#include "lite3.h"
#include "yyjson/yyjson.h"
#include <errno.h>
#include <limits.h>

_Static_assert(LITE3_NODE_ALIGNMENT == 4 && LITE3_JSON_NESTING_DEPTH_MAX == 32,
               "writer proof targets the pinned alignment and depth limits");

typedef int (*plasmite_grow_fn)(void *, unsigned char **, size_t *);

int plasmite_json_copy_growing(uintptr_t root, size_t parent,
    unsigned char **buf, size_t *len, size_t *capacity,
    void *state, plasmite_grow_fn grow, unsigned depth, int *json_native_safe)
{
    if (depth >= LITE3_JSON_NESTING_DEPTH_MAX) { errno = EINVAL; return -1; }
    yyjson_val *container = (yyjson_val *)root;
    bool object = yyjson_is_obj(container);
    yyjson_obj_iter objects;
    yyjson_arr_iter arrays;
    if (object) objects = yyjson_obj_iter_with(container);
    else arrays = yyjson_arr_iter_with(container);
    for (;;) {
        const char *key = NULL;
        yyjson_val *item;
        if (object) {
            yyjson_val *name = yyjson_obj_iter_next(&objects);
            if (!name) break;
            key = yyjson_get_str(name);
            item = yyjson_obj_iter_get_val(name);
        } else {
            item = yyjson_arr_iter_next(&arrays);
            if (!item) break;
        }
        plasmite_lite3_value value;
        plasmite_json_value_view((uintptr_t)item, &value);
        if (yyjson_is_uint(item)) {
            if (yyjson_get_uint(item) > INT64_MAX) { errno = ERANGE; return -1; }
            value.type = LITE3_TYPE_I64;
            value.integer = (int64_t)yyjson_get_uint(item);
        }
        if (value.type == LITE3_TYPE_I64 &&
                (value.integer < -9007199254740991LL || value.integer > 9007199254740991LL))
            *json_native_safe = 0;
        size_t child = 0;
        while (plasmite_lite3_put(*buf, len, *capacity, parent, key, &value, &child) < 0) {
            if (errno != ENOBUFS || grow(state, buf, capacity) < 0) return -1;
        }
        if (yyjson_is_ctn(item) && plasmite_json_copy_growing((uintptr_t)item,
                child, buf, len, capacity, state, grow, depth + 1, json_native_safe) < 0) return -1;
    }
    return 0;
}
