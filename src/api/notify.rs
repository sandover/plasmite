//! Purpose: Expose best-effort notification helpers for external callers.
//! Exports: `NotifyWait`, `wait_for_path`.
//! Role: Thin wrapper over core notify for CLI/bindings use.
//! Invariants: Wait results are best-effort; callers must fall back to polling.
//! Invariants: Unavailable notify never blocks progress.

use crate::core::notify::{WaitOutcome, open_for_path};
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotifyWait {
    Signaled,
    TimedOut,
    Unavailable,
}

pub fn wait_for_path(path: &Path, timeout: Duration) -> NotifyWait {
    match open_for_path(path) {
        Ok(semaphore) => match semaphore.wait(timeout) {
            Ok(WaitOutcome::Signaled) => NotifyWait::Signaled,
            Ok(WaitOutcome::TimedOut) => NotifyWait::TimedOut,
            Err(_) => NotifyWait::Unavailable,
        },
        Err(_) => NotifyWait::Unavailable,
    }
}
