/* macOS 14.4+ diagnostic only. Never use instrumented timing as a speed result. */
#include <os/os_sync_wait_on_address.h>
#include <semaphore.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>

#define INTERPOSE(replacement_fn, original_fn) \
    __attribute__((used)) static struct { const void *replacement; const void *original; } \
    interpose_##original_fn __attribute__((section("__DATA,__interpose"))) = { \
        (const void *)&replacement_fn, (const void *)&original_fn \
    };
static _Atomic uint64_t waits, wakes, posts, tries;
static _Atomic int active;

void plsm_bench_probe_begin(void) {
    atomic_store(&active, 0);
    atomic_store(&waits, 0); atomic_store(&wakes, 0);
    atomic_store(&posts, 0); atomic_store(&tries, 0);
    atomic_store(&active, 1);
}
const char *plsm_bench_probe_end(void) {
    atomic_store(&active, 0);
    static char result[256];
    snprintf(result, sizeof(result),
        "{\"address_wait_calls\":%llu,\"address_wake_all_calls\":%llu,"
        "\"sem_post_calls\":%llu,\"sem_trywait_calls\":%llu}",
        atomic_load(&waits), atomic_load(&wakes), atomic_load(&posts), atomic_load(&tries));
    return result;
}
static int measured_wait(void *address, uint64_t expected, size_t size,
                         os_sync_wait_on_address_flags_t flags, os_clockid_t clock,
                         uint64_t timeout) {
    if (atomic_load(&active)) atomic_fetch_add(&waits, 1);
    return os_sync_wait_on_address_with_timeout(address, expected, size, flags, clock, timeout);
}
static int measured_wake(void *address, size_t size, os_sync_wake_by_address_flags_t flags) {
    if (atomic_load(&active)) atomic_fetch_add(&wakes, 1);
    return os_sync_wake_by_address_all(address, size, flags);
}
static int measured_post(sem_t *semaphore) {
    if (atomic_load(&active)) atomic_fetch_add(&posts, 1);
    return sem_post(semaphore);
}
static int measured_try(sem_t *semaphore) {
    if (atomic_load(&active)) atomic_fetch_add(&tries, 1);
    return sem_trywait(semaphore);
}
INTERPOSE(measured_wait, os_sync_wait_on_address_with_timeout)
INTERPOSE(measured_wake, os_sync_wake_by_address_all)
INTERPOSE(measured_post, sem_post)
INTERPOSE(measured_try, sem_trywait)
