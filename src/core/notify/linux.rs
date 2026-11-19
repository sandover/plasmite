//! Linux shared-file wakeups. Storage locks and the full sequence remain authoritative.

use super::{NotifyError, WaitOutcome, map_notify_error_with};
use std::io;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

// One bit is enough for any number of sleepers. A departing reader may leave
// one harmless wake for the next writer without leaving a permanent count.
const ARMED: u32 = 1;
const FLAG_CHECK: Duration = Duration::from_millis(50);
pub(crate) const RECOVERY_CHECK: Duration = Duration::from_secs(1);

pub(crate) fn publish(word: &AtomicU32, sequence: u64) -> bool {
    let next = ((sequence as u32) << 1).to_le();
    word.swap(next, Ordering::AcqRel) & ARMED.to_le() != 0
}

pub(crate) fn wake(word: &AtomicU32) {
    wake_address(word, libc::FUTEX_WAKE);
}

pub(super) fn cancel(word: &AtomicU32) {
    if word.swap(1, Ordering::Release) == 0 {
        wake_address(word, libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG);
    }
}

fn wake_address(word: &AtomicU32, operation: libc::c_int) {
    // Hints can fail (for example under syscall filtering). Readers still
    // recheck storage at the recovery deadline.
    unsafe { libc::syscall(libc::SYS_futex, word.as_ptr(), operation, libc::c_int::MAX) };
}

pub(crate) fn wait(
    word: &AtomicU32,
    sequence: &AtomicU64,
    expected: u64,
    timeout: Duration,
    cancel: Option<&super::CancelSignal>,
) -> Result<WaitOutcome, NotifyError> {
    if sequence.load(Ordering::Acquire) != expected.to_le() {
        return Ok(WaitOutcome::Signaled);
    }
    if timeout.is_zero() {
        return Ok(WaitOutcome::TimedOut);
    }

    let before = word.load(Ordering::Acquire);
    let armed = before | ARMED.to_le();
    if before != armed
        && word
            .compare_exchange(before, armed, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return Ok(WaitOutcome::Signaled);
    }
    // Registration and the final sequence check cover writes racing with
    // entry into the kernel, including writers that leave the wake word alone.
    if sequence.load(Ordering::Acquire) != expected.to_le() {
        return Ok(WaitOutcome::Signaled);
    }

    if let Some(cancel) = cancel {
        if cancel.flag.load(Ordering::Acquire) {
            return Ok(WaitOutcome::Signaled);
        }
        if let Some(outcome) = wait_with_cancel(word, armed, timeout, &cancel.event)? {
            return Ok(outcome);
        }
    }

    let timeout = if cancel.is_some() {
        timeout.min(FLAG_CHECK)
    } else {
        timeout
    };
    // The original futex syscall uses kernel long fields (including on
    // 32-bit Linux, where libc timespec can use 64-bit time_t).
    #[repr(C)]
    struct OldTimespec {
        secs: libc::c_long,
        nanos: libc::c_long,
    }
    let timeout = OldTimespec {
        secs: timeout
            .as_secs()
            .try_into()
            .map_err(|_| NotifyError::Unavailable)?,
        nanos: timeout.subsec_nanos() as libc::c_long,
    };
    let result = unsafe {
        libc::syscall(
            libc::SYS_futex,
            word.as_ptr(),
            libc::FUTEX_WAIT,
            armed,
            &timeout,
        )
    };
    wait_result(result)
}

fn wait_with_cancel(
    word: &AtomicU32,
    armed: u32,
    duration: Duration,
    cancel: &AtomicU32,
) -> Result<Option<WaitOutcome>, NotifyError> {
    // futex_waitv compares both words before parking. Its timeout uses an
    // absolute CLOCK_MONOTONIC deadline and a fixed 64-bit kernel layout.
    #[repr(C)]
    struct Waiter {
        value: u64,
        address: u64,
        flags: u32,
        reserved: u32,
    }
    #[repr(C)]
    struct Deadline {
        secs: i64,
        nanos: i64,
    }
    const FUTEX_32: u32 = 2;
    let waits = [
        Waiter {
            value: armed.into(),
            address: word.as_ptr() as usize as u64,
            flags: FUTEX_32,
            reserved: 0,
        },
        Waiter {
            value: 0,
            address: cancel.as_ptr() as usize as u64,
            flags: FUTEX_32 | libc::FUTEX_PRIVATE_FLAG as u32,
            reserved: 0,
        },
    ];
    let mut clock = std::mem::MaybeUninit::<libc::timespec>::uninit();
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, clock.as_mut_ptr()) } != 0 {
        return Err(map_notify_error_with(io::Error::last_os_error()));
    }
    let clock = unsafe { clock.assume_init() };
    let deadline = Duration::new(clock.tv_sec as u64, clock.tv_nsec as u32)
        .checked_add(duration)
        .ok_or(NotifyError::Unavailable)?;
    let deadline = Deadline {
        secs: deadline
            .as_secs()
            .try_into()
            .map_err(|_| NotifyError::Unavailable)?,
        nanos: deadline.subsec_nanos().into(),
    };
    let result = unsafe {
        libc::syscall(
            libc::SYS_futex_waitv,
            waits.as_ptr(),
            waits.len(),
            0,
            &deadline,
            libc::CLOCK_MONOTONIC,
        )
    };
    if result < 0
        && matches!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOSYS | libc::EPERM | libc::EACCES)
        )
    {
        return Ok(None);
    }
    wait_result(result).map(Some)
}

fn wait_result(result: libc::c_long) -> Result<WaitOutcome, NotifyError> {
    if result >= 0 {
        return Ok(WaitOutcome::Signaled);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EAGAIN | libc::EINTR) => Ok(WaitOutcome::Signaled),
        Some(libc::ETIMEDOUT) => Ok(WaitOutcome::TimedOut),
        Some(libc::EPERM | libc::EACCES) => Err(NotifyError::Unavailable),
        _ => Err(map_notify_error_with(error)),
    }
}
