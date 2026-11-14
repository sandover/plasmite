/* Benchmark helper: sample another process without ps's centisecond rounding.
 * Build: cc -O2 scripts/process_cpu_macos.c -o /tmp/plasmite-process-cpu
 * XNU fill_taskprocinfo reports recount_times_mach; convert its Mach ticks:
 * https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/bsd_kern.c
 */
#include <errno.h>
#include <inttypes.h>
#include <limits.h>
#include <libproc.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/proc_info.h>

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    char *end;
    errno = 0;
    long pid = strtol(argv[1], &end, 10);
    if (errno || *end || pid <= 0 || pid > INT_MAX) return 2;
    struct proc_taskinfo info;
    if (proc_pidinfo((int)pid, PROC_PIDTASKINFO, 0, &info, sizeof(info)) != sizeof(info)) {
        perror("proc_pidinfo");
        return 1;
    }
    mach_timebase_info_data_t scale;
    if (mach_timebase_info(&scale) != KERN_SUCCESS) return 1;
    __uint128_t ticks = (__uint128_t)info.pti_total_user + info.pti_total_system;
    printf("%" PRIu64 "\n", (uint64_t)(ticks * scale.numer / scale.denom));
    return 0;
}
