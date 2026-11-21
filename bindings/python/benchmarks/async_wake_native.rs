//! Private benchmark shim for Plasmite's native pool notification.

use plasmite::api::notify::{NotifyHandle, NotifyWait, open_for_path};
use std::ffi::{CStr, c_char};
use std::time::Duration;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn probe_open(path: *const c_char) -> *mut NotifyHandle {
    // The Python probe supplies a valid NUL-terminated UTF-8 path.
    let path = unsafe { CStr::from_ptr(path) };
    path.to_str()
        .ok()
        .and_then(|path| open_for_path(path.as_ref()))
        .map_or(std::ptr::null_mut(), |handle| {
            Box::into_raw(Box::new(handle))
        })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn probe_wait(handle: *mut NotifyHandle) -> i32 {
    // The Python probe grants its worker exclusive access until it joins.
    match unsafe { &mut *handle }.wait(Duration::from_secs(1)) {
        NotifyWait::Signaled => 1,
        NotifyWait::TimedOut => 0,
        NotifyWait::Unavailable => -1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn probe_close(handle: *mut NotifyHandle) {
    // The worker has joined and this is the only release of the handle.
    drop(unsafe { Box::from_raw(handle) });
}
