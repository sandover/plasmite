//! A one-shot event for each sequence that readers wait on.

use super::{CancelSignal, NotifyError, WaitOutcome};
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
use windows_sys::Win32::Foundation::{WAIT_FAILED, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    CREATE_EVENT_MANUAL_RESET, CreateEventExW, EVENT_MODIFY_STATE, OpenEventW,
    SYNCHRONIZATION_SYNCHRONIZE, SetEvent, WaitForMultipleObjects,
};

fn event_name(identity: &str, sequence: u64) -> Vec<u16> {
    // The global namespace lets same-file service and interactive processes
    // open the same hint when their permissions allow it.
    format!(r"Global\Plasmite.Follow.{identity}.{sequence:x}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

fn owned(raw: windows_sys::Win32::Foundation::HANDLE) -> Option<OwnedHandle> {
    (!raw.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(raw) })
}

fn event(name: *const u16, access: u32) -> Option<OwnedHandle> {
    owned(unsafe { CreateEventExW(std::ptr::null(), name, CREATE_EVENT_MANUAL_RESET, access) })
}

pub(super) fn cancel_event() -> Option<OwnedHandle> {
    event(
        std::ptr::null(),
        SYNCHRONIZATION_SYNCHRONIZE | EVENT_MODIFY_STATE,
    )
}

pub(super) fn signal(event: &OwnedHandle) {
    unsafe { SetEvent(event.as_raw_handle()) };
}

pub(crate) fn wake(identity: &str, previous: u64) {
    let name = event_name(identity, previous);
    if let Some(event) = owned(unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) }) {
        signal(&event);
    }
}

pub(crate) fn wait(
    identity: &str,
    interest: &AtomicU32,
    sequence: &AtomicU64,
    expected: u64,
    timeout: Duration,
    cancel: Option<&CancelSignal>,
) -> Result<WaitOutcome, NotifyError> {
    if sequence.load(Ordering::Acquire) != expected.to_le() {
        return Ok(WaitOutcome::Signaled);
    }
    if timeout.is_zero() {
        return Ok(WaitOutcome::TimedOut);
    }
    let name = event_name(identity, expected);
    let event =
        event(name.as_ptr(), SYNCHRONIZATION_SYNCHRONIZE).ok_or(NotifyError::Unavailable)?;
    // Open the event before arming. A writer can now signal it even if this
    // reader has yet to enter the wait. Each generation gets a fresh event.
    if super::arm(interest).is_none() || sequence.load(Ordering::Acquire) != expected.to_le() {
        return Ok(WaitOutcome::Signaled);
    }
    let mut handles = [event.as_raw_handle(), std::ptr::null_mut()];
    let mut count = 1;
    let mut timeout = timeout;
    if let Some(cancel) = cancel {
        let cancel_event = cancel.event.get_or_init(cancel_event);
        if cancel.flag.load(Ordering::Acquire) {
            return Ok(WaitOutcome::Signaled);
        }
        if let Some(cancel_event) = cancel_event {
            handles[1] = cancel_event.as_raw_handle();
            count = 2;
        } else {
            timeout = timeout.min(super::FLAG_CHECK);
        }
    }
    let millis = timeout
        .as_nanos()
        .div_ceil(1_000_000)
        .min(u128::from(u32::MAX - 1)) as u32;
    match unsafe { WaitForMultipleObjects(count, handles.as_ptr(), 0, millis) } {
        WAIT_TIMEOUT => Ok(WaitOutcome::TimedOut),
        WAIT_FAILED => Err(NotifyError::Io(io::Error::last_os_error())),
        _ => Ok(WaitOutcome::Signaled),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    #[test]
    fn delayed_signal_only_wakes_its_own_generation() {
        let identity = format!(
            "test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        );
        // Include more than 32 sequence bits in both names.
        let previous = u64::from(u32::MAX) + 20;
        let next = previous + 1;
        let open = |sequence| {
            let name = event_name(&identity, sequence);
            event(name.as_ptr(), SYNCHRONIZATION_SYNCHRONIZE).unwrap()
        };
        let first = [open(previous), open(previous)];
        let second = open(next);

        // The old writer signals after the next generation's reader registered.
        // The first readers have opened the event but have yet to park.
        wake(&identity, previous);
        for reader in &first {
            assert_eq!(
                unsafe { WaitForSingleObject(reader.as_raw_handle(), 0) },
                WAIT_OBJECT_0
            );
        }
        assert_eq!(
            unsafe { WaitForSingleObject(second.as_raw_handle(), 0) },
            WAIT_TIMEOUT
        );
        wake(&identity, next);
        assert_eq!(
            unsafe { WaitForSingleObject(second.as_raw_handle(), 0) },
            WAIT_OBJECT_0
        );
    }
}
