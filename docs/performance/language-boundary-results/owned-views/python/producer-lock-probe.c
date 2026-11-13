/* macOS diagnostic only: counts and inclusive syscall budgets, never timing evidence. */
#include <errno.h>
#include <os/os_sync_wait_on_address.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/file.h>
#include <time.h>

#define INTERPOSE(replacement, original)                                                           \
    __attribute__((used)) static struct {                                                          \
        const void *new_fn, *old_fn;                                                               \
    } map_##original __attribute__((section("__DATA,__interpose"))) = {(const void *)replacement,  \
                                                                       (const void *)original};
enum { SHARED, EXCLUSIVE, UNLOCK, WAIT, WAKE, SLOTS };
static _Atomic uint64_t calls[SLOTS], elapsed[SLOTS], max_ns[SLOTS];
static _Atomic int active;
static uint64_t now(void) {
    struct timespec time;
    clock_gettime(CLOCK_MONOTONIC, &time);
    return (uint64_t)time.tv_sec * 1000000000 + time.tv_nsec;
}
static void record(int slot, uint64_t start) {
    uint64_t ns = now() - start;
    atomic_fetch_add(&calls[slot], 1);
    atomic_fetch_add(&elapsed[slot], ns);
    uint64_t previous = atomic_load(&max_ns[slot]);
    while (previous < ns && !atomic_compare_exchange_weak(&max_ns[slot], &previous, ns)) {
    }
}
void plsm_bench_probe_begin(void) {
    atomic_store(&active, 0);
    for (int i = 0; i < SLOTS; ++i) {
        atomic_store(&calls[i], 0);
        atomic_store(&elapsed[i], 0);
        atomic_store(&max_ns[i], 0);
    }
    atomic_store(&active, 1);
}
const char *plsm_bench_probe_end(void) {
    atomic_store(&active, 0);
    static char result[1200];
    const char *names[] = {"shared_lock", "exclusive_lock", "unlock", "address_wait",
                           "address_wake"};
    size_t length = 0;
    length += (size_t)snprintf(result + length, sizeof(result) - length, "{");
    for (int i = 0; i < SLOTS; ++i)
        length += (size_t)snprintf(
            result + length, sizeof(result) - length,
            "%s\"%s\":{\"calls\":%llu,\"inclusive_ns\":%llu,\"max_ns\":%llu}", i ? "," : "",
            names[i], atomic_load(&calls[i]), atomic_load(&elapsed[i]), atomic_load(&max_ns[i]));
    snprintf(result + length, sizeof(result) - length, "}");
    return result;
}
static int measured_flock(int fd, int operation) {
    int running = atomic_load(&active), slot = (operation & ~LOCK_NB) == LOCK_SH   ? SHARED
                                               : (operation & ~LOCK_NB) == LOCK_EX ? EXCLUSIVE
                                                                                   : UNLOCK;
    uint64_t start = running ? now() : 0;
    int result = flock(fd, operation);
    int saved_errno = errno;
    if (running)
        record(slot, start);
    errno = saved_errno;
    return result;
}
static int measured_wait(void *address, uint64_t expected, size_t size,
                         os_sync_wait_on_address_flags_t flags, os_clockid_t clock,
                         uint64_t timeout) {
    int running = atomic_load(&active);
    uint64_t start = running ? now() : 0;
    int result =
        os_sync_wait_on_address_with_timeout(address, expected, size, flags, clock, timeout);
    int saved_errno = errno;
    if (running)
        record(WAIT, start);
    errno = saved_errno;
    return result;
}
static int measured_wake(void *address, size_t size, os_sync_wake_by_address_flags_t flags) {
    int running = atomic_load(&active);
    uint64_t start = running ? now() : 0;
    int result = os_sync_wake_by_address_all(address, size, flags);
    int saved_errno = errno;
    if (running)
        record(WAKE, start);
    errno = saved_errno;
    return result;
}
INTERPOSE(measured_flock, flock)
INTERPOSE(measured_wait, os_sync_wait_on_address_with_timeout)
INTERPOSE(measured_wake, os_sync_wake_by_address_all)
