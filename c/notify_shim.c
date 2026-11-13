/* Public Darwin shared-address waits. Older SDKs and OS versions keep polling. */
#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#if __has_include(<os/os_sync_wait_on_address.h>)
#include <os/os_sync_wait_on_address.h>
#define HAVE_SHARED_ADDRESS_WAIT 1
#endif

int plasmite_address_wait_available(void)
{
#ifdef HAVE_SHARED_ADDRESS_WAIT
        if (__builtin_available(macOS 14.4, *)) return 1;
#endif
        return 0;
}

int plasmite_address_wait(void *address, uint64_t expected, uint64_t timeout_ns)
{
#ifdef HAVE_SHARED_ADDRESS_WAIT
        if (__builtin_available(macOS 14.4, *)) {
                return os_sync_wait_on_address_with_timeout(address, expected, 8,
                        OS_SYNC_WAIT_ON_ADDRESS_SHARED, OS_CLOCK_MACH_ABSOLUTE_TIME,
                        timeout_ns);
        }
#endif
        (void)address; (void)expected; (void)timeout_ns;
        errno = ENOSYS;
        return -1;
}

int plasmite_address_wake_all(void *address)
{
#ifdef HAVE_SHARED_ADDRESS_WAIT
        if (__builtin_available(macOS 14.4, *)) {
                return os_sync_wake_by_address_all(address, 8, OS_SYNC_WAKE_BY_ADDRESS_SHARED);
        }
#endif
        (void)address;
        errno = ENOSYS;
        return -1;
}
