#define PLSM_INTERNAL_ABI
#include "plasmite.h"
typedef struct {
    int status;
    plsm_error_t *error;
    plsm_udp_event_t event;
    const plsm_message_view_t *owner;
} plsm_go_udp_next_t;
static inline plsm_go_udp_next_t plsm_go_udp_next(plsm_udp_follow_t *follow,uint64_t wait) {
    plsm_go_udp_next_t result={0};
    result.status=plsm_udp_follow_next_view(follow,wait,&result.event,&result.owner,&result.error);
    return result;
}
static inline void plsm_go_udp_event_destroy(plsm_udp_event_t event) { plsm_udp_event_free(&event); }
