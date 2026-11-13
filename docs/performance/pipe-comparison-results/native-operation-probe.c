/* macOS-only native-call diagnostic for the controlled benchmark processes.
 * Inject at process startup. Report counts separately from normal timing runs.
 */
#include <fcntl.h>
// dyld consumes address pairs in this Mach-O section. Keep the probe local
// to these benchmark processes; it is not a general instrumentation library.
#define DYLD_INTERPOSE(replacement_fn, original_fn) \
    __attribute__((used)) static struct { const void *replacement; const void *original; } \
    interpose_##original_fn __attribute__((section("__DATA,__interpose"))) = { \
        (const void *)&replacement_fn, (const void *)&original_fn \
    };
#include <semaphore.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/file.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

enum { RD, RDB, WR, WRB, WRMAX, SH, EX, UN, CL, ST, FST, SO, SC, SP, SW, MS, TOTAL };
static _Atomic uint64_t counts[TOTAL];
static _Atomic int active;
#define ADD(index, amount) atomic_fetch_add_explicit(&counts[index], amount, memory_order_relaxed)
#define ACTIVE atomic_load_explicit(&active, memory_order_relaxed)

void plsm_bench_probe_begin(void) {
    atomic_store(&active, 0);
    for (int i = 0; i < TOTAL; i++) atomic_store(&counts[i], 0);
    atomic_store(&active, 1);
}

const char *plsm_bench_probe_end(void) {
    atomic_store(&active, 0);
    static char result[1024];
    uint64_t c[TOTAL];
    for (int i = 0; i < TOTAL; i++) c[i] = atomic_load(&counts[i]);
    snprintf(result, sizeof(result),
        "{\"read_calls\":%llu,\"read_bytes\":%llu,\"write_calls\":%llu,\"write_bytes\":%llu,"
        "\"max_write_bytes\":%llu,\"shared_locks\":%llu,\"exclusive_locks\":%llu,\"unlocks\":%llu,"
        "\"close_calls\":%llu,\"stat_calls\":%llu,\"fstat_calls\":%llu,\"sem_open_calls\":%llu,"
        "\"sem_close_calls\":%llu,\"sem_post_calls\":%llu,\"sem_trywait_calls\":%llu,\"msync_calls\":%llu}",
        c[RD],c[RDB],c[WR],c[WRB],c[WRMAX],c[SH],c[EX],c[UN],c[CL],c[ST],c[FST],c[SO],c[SC],c[SP],c[SW],c[MS]);
    return result;
}

static ssize_t measured_read(int fd, void *buf, size_t len) {
    ssize_t result = read(fd, buf, len);
    if (ACTIVE) { ADD(RD, 1); if (result > 0) ADD(RDB, (uint64_t)result); }
    return result;
}
static ssize_t measured_write(int fd, const void *buf, size_t len) {
    ssize_t result = write(fd, buf, len);
    if (ACTIVE) {
        ADD(WR, 1);
        if (result > 0) {
            ADD(WRB, (uint64_t)result);
            uint64_t old = atomic_load(&counts[WRMAX]);
            while (old < (uint64_t)result && !atomic_compare_exchange_weak(&counts[WRMAX], &old, (uint64_t)result)) {}
        }
    }
    return result;
}
static int measured_close(int fd) { if (ACTIVE) ADD(CL,1); return close(fd); }
static int measured_stat(const char *path, struct stat *out) { if (ACTIVE) ADD(ST,1); return stat(path,out); }
static int measured_fstat(int fd, struct stat *out) { if (ACTIVE) ADD(FST,1); return fstat(fd,out); }
static int measured_flock(int fd, int operation) {
    if (ACTIVE) {
        int kind = operation & ~LOCK_NB;
        if (kind == LOCK_SH) ADD(SH,1);
        if (kind == LOCK_EX) ADD(EX,1);
        if (kind == LOCK_UN) ADD(UN,1);
    }
    return flock(fd,operation);
}
static sem_t *measured_sem_open(const char *name, int flags, ...) {
    if (ACTIVE) ADD(SO,1);
    if (!(flags & O_CREAT)) return sem_open(name,flags);
    va_list args;
    va_start(args,flags);
    unsigned int mode = va_arg(args,unsigned int);
    unsigned int value = va_arg(args,unsigned int);
    va_end(args);
    return sem_open(name,flags,mode,value);
}
static int measured_sem_close(sem_t *sem) { if (ACTIVE) ADD(SC,1); return sem_close(sem); }
static int measured_sem_post(sem_t *sem) { if (ACTIVE) ADD(SP,1); return sem_post(sem); }
static int measured_sem_trywait(sem_t *sem) { if (ACTIVE) ADD(SW,1); return sem_trywait(sem); }
static int measured_msync(void *addr, size_t len, int flags) { if (ACTIVE) ADD(MS,1); return msync(addr,len,flags); }

DYLD_INTERPOSE(measured_read,read)
DYLD_INTERPOSE(measured_write,write)
DYLD_INTERPOSE(measured_close,close)
DYLD_INTERPOSE(measured_stat,stat)
DYLD_INTERPOSE(measured_fstat,fstat)
DYLD_INTERPOSE(measured_flock,flock)
DYLD_INTERPOSE(measured_sem_open,sem_open)
DYLD_INTERPOSE(measured_sem_close,sem_close)
DYLD_INTERPOSE(measured_sem_post,sem_post)
DYLD_INTERPOSE(measured_sem_trywait,sem_trywait)
DYLD_INTERPOSE(measured_msync,msync)
