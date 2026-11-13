/* macOS allocation-entry counts for controlled workers, never speed timing. */
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#define INTERPOSE(replacement_fn, original_fn) \
    __attribute__((used)) static struct { const void *replacement; const void *original; } \
    interpose_##original_fn __attribute__((section("__DATA,__interpose"))) = { \
        (const void *)&replacement_fn, (const void *)&original_fn \
    };
static _Atomic uint64_t allocations, requested_bytes, frees;
static _Atomic int active;
static void count(size_t size) {
    if (atomic_load(&active)) {
        atomic_fetch_add(&allocations,1); atomic_fetch_add(&requested_bytes,size);
    }
}
void plsm_bench_probe_begin(void) {
    atomic_store(&active,0);
    atomic_store(&allocations,0); atomic_store(&requested_bytes,0); atomic_store(&frees,0);
    atomic_store(&active,1);
}
const char *plsm_bench_probe_end(void) {
    atomic_store(&active,0);
    static char result[256];
    snprintf(result,sizeof(result),"{\"allocation_calls\":%llu,\"requested_bytes\":%llu,\"free_calls\":%llu}",
             atomic_load(&allocations),atomic_load(&requested_bytes),atomic_load(&frees));
    return result;
}
static void *measured_malloc(size_t size) { count(size); return malloc(size); }
static void *measured_calloc(size_t n,size_t size) {
    count(size && n>SIZE_MAX/size ? 0 : n*size); return calloc(n,size);
}
static void *measured_realloc(void *ptr,size_t size) { count(size); return realloc(ptr,size); }
static void measured_free(void *ptr) {
    if (atomic_load(&active)) atomic_fetch_add(&frees,1);
    free(ptr);
}
INTERPOSE(measured_malloc,malloc)
INTERPOSE(measured_calloc,calloc)
INTERPOSE(measured_realloc,realloc)
INTERPOSE(measured_free,free)
