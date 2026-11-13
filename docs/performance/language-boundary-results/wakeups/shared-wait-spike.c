#include <os/os_sync_wait_on_address.h>
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static uint64_t now_ns(void) {
    struct timespec t;
    assert(clock_gettime(CLOCK_MONOTONIC,&t)==0);
    return (uint64_t)t.tv_sec*1000000000ULL+t.tv_nsec;
}
static void receive(int fd, void *out, size_t len) {
    while (len) { ssize_t n=read(fd,out,len); assert(n>0); out=(char*)out+n; len-=(size_t)n; }
}
struct outcome { int result; int error; uint64_t elapsed_ns; uint64_t value; uintptr_t address; };
int main(void) {
    if (__builtin_available(macOS 14.4,*)) {} else { return 77; }
    char name[]="/private/tmp/plasmite-shared-wait-XXXXXX";
    int fd=mkstemp(name); assert(fd>=0); assert(ftruncate(fd,4096)==0);
    _Atomic uint64_t *word=mmap(NULL,4096,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);
    assert(word!=MAP_FAILED); atomic_init(word,0); assert(atomic_is_lock_free(word));
    int ready[2], results[2]; assert(pipe(ready)==0 && pipe(results)==0);
    pid_t children[3];
    for(int i=0;i<3;i++) {
        children[i]=fork(); assert(children[i]>=0);
        if(children[i]==0) {
            close(ready[0]); close(results[0]);
            int own=open(name,O_RDWR); assert(own>=0);
            _Atomic uint64_t *alias=mmap(NULL,4096,PROT_READ|PROT_WRITE,MAP_SHARED,own,0);
            assert(alias!=MAP_FAILED && alias!=word); close(own);
            assert(atomic_load_explicit(alias,memory_order_acquire)==0);
            assert(write(ready[1],"r",1)==1); close(ready[1]);
            uint64_t started=now_ns(); errno=0;
            int r=os_sync_wait_on_address_with_timeout(alias,0,8,OS_SYNC_WAIT_ON_ADDRESS_SHARED,
                    OS_CLOCK_MACH_ABSOLUTE_TIME,1000000000ULL);
            int error=errno;
            struct outcome out={r,error,now_ns()-started,atomic_load_explicit(alias,memory_order_acquire),(uintptr_t)alias};
            assert(write(results[1],&out,sizeof out)==sizeof out);
            munmap(alias,4096); munmap(word,4096); close(fd); close(results[1]);
            _exit(r>=0 && out.value==1 ? 0 : 1);
        }
    }
    close(ready[1]); close(results[1]);
    char markers[3]; receive(ready[0],markers,sizeof markers); close(ready[0]);
    usleep(20000);
    atomic_store_explicit(word,1,memory_order_release);
    errno=0;
    int woke=os_sync_wake_by_address_all(word,8,OS_SYNC_WAKE_BY_ADDRESS_SHARED);
    fprintf(stderr,"wake return=%d errno=%d\n",woke,errno);
    struct outcome outcomes[3]; receive(results[0],outcomes,sizeof outcomes); close(results[0]);
    int all_ok=1;
    for(int i=0;i<3;i++) {
        int status; assert(waitpid(children[i],&status,0)==children[i]);
        fprintf(stderr,"child %d return=%d errno=%d value=%llu wait_ns=%llu status=%d\n",i,outcomes[i].result,outcomes[i].error,outcomes[i].value,outcomes[i].elapsed_ns,status);
        if(!(WIFEXITED(status) && WEXITSTATUS(status)==0)) all_ok=0;
    }
    uint64_t started=now_ns(); errno=0;
    int timeout=os_sync_wait_on_address_with_timeout(word,1,8,OS_SYNC_WAIT_ON_ADDRESS_SHARED,
        OS_CLOCK_MACH_ABSOLUTE_TIME,1000000);
    int timeout_error=errno; uint64_t timeout_elapsed=now_ns()-started;
    fprintf(stderr,"timeout return=%d errno=%d\n",timeout,timeout_error);
    started=now_ns(); errno=0;
    int changed=os_sync_wait_on_address_with_timeout(word,0,8,OS_SYNC_WAIT_ON_ADDRESS_SHARED,
        OS_CLOCK_MACH_ABSOLUTE_TIME,1000000000);
    uint64_t changed_elapsed=now_ns()-started;
    fprintf(stderr,"changed return=%d errno=%d\n",changed,errno);
    printf("{\"children\":3,\"different_mappings\":true,\"wake_all_result\":%d,\"timeout_ns\":%llu,\"changed_value_ns\":%llu,\"waits_ns\":[%llu,%llu,%llu]}\n",
        woke,timeout_elapsed,changed_elapsed,outcomes[0].elapsed_ns,outcomes[1].elapsed_ns,outcomes[2].elapsed_ns);
    assert(munmap(word,4096)==0); close(fd); assert(unlink(name)==0);
    return woke>=0 && all_ok && timeout==-1 && timeout_error==ETIMEDOUT && changed>=0 ? 0 : 1;
}
