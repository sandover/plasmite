#include "plasmite.h"
typedef struct {
    const uint8_t *bytes;
    size_t len, data_node, meta_node, duplicate_count;
    uint64_t seq, timestamp_ns;
} plsm_go_owned_info_t;
plsm_go_owned_info_t plsm_go_view_info(const plsm_message_view_t *owner);
size_t plsm_go_view_duplicates(const plsm_message_view_t *owner, size_t *out, size_t capacity);
