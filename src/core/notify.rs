//! Best-effort local pool notifications. Readers always verify committed
//! storage and their own position after a wake-up or a timeout.

use std::io;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use std::sync::atomic::AtomicU32;
#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use std::time::Duration;

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(windows)]
pub(crate) mod windows;

#[cfg(any(target_os = "linux", windows))]
pub(crate) const FLAG_CHECK: Duration = Duration::from_millis(50);
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(crate) const RECOVERY_CHECK: Duration = Duration::from_secs(1);
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(crate) const ARMED: u32 = 1u32.to_le();

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(crate) fn publish(word: &AtomicU32, sequence: u64) -> bool {
    // One bit covers every sleeper. A departing reader can leave at most
    // one extra wake for the next writer.
    word.swap(((sequence as u32) << 1).to_le(), Ordering::AcqRel) & ARMED != 0
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(crate) fn arm(word: &AtomicU32) -> Option<u32> {
    let before = word.load(Ordering::Acquire);
    let armed = before | ARMED;
    if before != armed {
        word.compare_exchange(before, armed, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
    }
    Some(armed)
}

/// Bindings own the cancellation operation, so they can wake a parked local
/// reader. The flag also serves remote readers and older notification backends.
#[derive(Debug, Default)]
pub(crate) struct CancelSignal {
    flag: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(target_os = "linux")]
    event: std::sync::atomic::AtomicU32,
    #[cfg(windows)]
    event: std::sync::OnceLock<Option<std::os::windows::io::OwnedHandle>>,
}

impl CancelSignal {
    pub(crate) fn flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.flag)
    }

    pub(crate) fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        #[cfg(target_os = "linux")]
        linux::cancel(&self.event);
        #[cfg(windows)]
        if let Some(event) = self.event.get_or_init(windows::cancel_event) {
            windows::signal(event);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
// The polling fallback keeps this shared result type but cannot produce semaphore events.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
pub(crate) enum WaitOutcome {
    Signaled,
    TimedOut,
}

#[derive(Debug)]
pub(crate) enum NotifyError {
    Unavailable,
    // Native Unix notification operations can report operating-system errors.
    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
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
    #[cfg(any(target_os = "linux", windows))]
    {
        true
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        false
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn wait_for_sequence(
    interest: &AtomicU32,
    word: &AtomicU64,
    expected: u64,
    timeout: Duration,
) -> Result<WaitOutcome, NotifyError> {
    if word.load(Ordering::Acquire) != expected.to_le() {
        return Ok(WaitOutcome::Signaled);
    }
    if timeout.is_zero() {
        return Ok(WaitOutcome::TimedOut);
    }
    if arm(interest).is_none() || word.load(Ordering::Acquire) != expected.to_le() {
        return Ok(WaitOutcome::Signaled);
    }
    // Retain the sequence as the wait address so older writers can wake it.
    let nanos = timeout.as_nanos().min(u64::MAX as u128) as u64;
    let result = unsafe { plasmite_address_wait(word.as_ptr(), expected.to_le(), nanos) };
    if result >= 0 {
        return Ok(WaitOutcome::Signaled);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ETIMEDOUT) => Ok(WaitOutcome::TimedOut),
        Some(libc::EINTR) => Ok(WaitOutcome::Signaled),
        _ => Err(map_notify_error_with(error)),
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn wake_sequence(word: &AtomicU64) {
    // The reader's bounded check covers any failed hint.
    unsafe { plasmite_address_wake_all(word.as_ptr()) };
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

#[cfg(not(any(target_os = "linux", windows)))]
mod semaphore;
#[cfg(not(any(target_os = "linux", windows)))]
pub(crate) use semaphore::{PoolSemaphore, open_for_path, pool_semaphore_name, post_for_name};
