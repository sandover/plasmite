//! Best-effort local pool notifications. Readers always verify committed
//! storage and their own position after a wake-up or a timeout.

use std::io;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(not(target_os = "linux"))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
#[cfg(not(target_os = "linux"))]
use std::time::Duration;

#[cfg(target_os = "linux")]
pub(crate) mod linux;

/// Bindings own the cancellation operation, so they can wake a parked local
/// reader. The flag also serves remote readers and older notification backends.
#[derive(Debug, Default)]
pub(crate) struct CancelSignal {
    flag: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(target_os = "linux")]
    event: std::sync::atomic::AtomicU32,
}

impl CancelSignal {
    pub(crate) fn flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.flag)
    }

    pub(crate) fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        #[cfg(target_os = "linux")]
        linux::cancel(&self.event);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
// The polling fallback keeps this shared result type but cannot produce semaphore events.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) enum WaitOutcome {
    Signaled,
    TimedOut,
}

#[derive(Debug)]
pub(crate) enum NotifyError {
    Unavailable,
    // Native Unix notification operations can report operating-system errors.
    #[cfg_attr(not(unix), allow(dead_code))]
    Io(io::Error),
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn plasmite_address_wait_available() -> libc::c_int;
    fn plasmite_address_wait(address: *mut u64, expected: u64, timeout_ns: u64) -> libc::c_int;
    fn plasmite_address_wake_all(address: *mut u64) -> libc::c_int;
}

pub(crate) fn shared_address_available() -> bool {
    #[cfg(test)]
    if FORCE_UNAVAILABLE.load(Ordering::SeqCst) {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *AVAILABLE.get_or_init(|| unsafe { plasmite_address_wait_available() != 0 })
    }
    #[cfg(target_os = "linux")]
    {
        true
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn wait_for_sequence(
    _word: &AtomicU64,
    _expected: u64,
    _timeout: Duration,
) -> Result<WaitOutcome, NotifyError> {
    #[cfg(target_os = "macos")]
    {
        // Touch this mapping before entering the kernel. This also handles a
        // publication that wins the race after the cursor's empty-state check.
        if _word.load(std::sync::atomic::Ordering::Acquire) != _expected.to_le() {
            return Ok(WaitOutcome::Signaled);
        }
        if _timeout.is_zero() {
            return Ok(WaitOutcome::TimedOut);
        }
        let nanos = _timeout.as_nanos().min(u64::MAX as u128) as u64;
        // The borrowed atomic keeps the aligned mapped word alive through this
        // compare-and-wait; the C bridge reads it and guards OS availability.
        let ret = unsafe { plasmite_address_wait(_word.as_ptr(), _expected.to_le(), nanos) };
        if ret >= 0 {
            return Ok(WaitOutcome::Signaled);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ETIMEDOUT) => Ok(WaitOutcome::TimedOut),
            Some(libc::EINTR) => Ok(WaitOutcome::Signaled),
            _ => Err(map_notify_error_with(error)),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(NotifyError::Unavailable)
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn post_for_pool(
    _word: &AtomicU64,
    name: &str,
    cached: &mut Option<PoolSemaphore>,
) -> Result<(), NotifyError> {
    #[cfg(target_os = "macos")]
    if shared_address_available() {
        let ret = unsafe { plasmite_address_wake_all(_word.as_ptr()) };
        if ret >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(()) // No waiter currently needs a hint.
        } else {
            Err(map_notify_error_with(error))
        };
    }
    post_for_name(name, cached)
}

#[cfg(test)]
pub(crate) fn force_unavailable_for_tests(enabled: bool) {
    FORCE_UNAVAILABLE.store(enabled, Ordering::SeqCst);
}

#[cfg(test)]
static FORCE_UNAVAILABLE: AtomicBool = AtomicBool::new(false);

#[cfg(all(unix, not(target_os = "linux")))]
fn map_notify_error() -> NotifyError {
    map_notify_error_with(io::Error::last_os_error())
}

#[cfg(unix)]
fn map_notify_error_with(err: io::Error) -> NotifyError {
    match err.raw_os_error() {
        Some(code) if code == libc::ENOSYS || code == libc::ENOTSUP => NotifyError::Unavailable,
        _ => NotifyError::Io(err),
    }
}

#[cfg(not(target_os = "linux"))]
mod semaphore;
#[cfg(not(target_os = "linux"))]
use semaphore::post_for_name;
#[cfg(not(target_os = "linux"))]
pub(crate) use semaphore::{PoolSemaphore, open_for_path, pool_semaphore_name};
