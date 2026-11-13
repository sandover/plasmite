//! Purpose: Expose best-effort notification helpers for external callers.
//! Exports: `NotifyWait`, `wait_for_path`.
//! Role: Thin wrapper over core notify for CLI/bindings use.
//! Invariants: Wait results are best-effort; callers must fall back to polling.
//! Invariants: Unavailable notify never blocks progress.

use crate::core::notify::{NotifyError, WaitOutcome, open_for_path as core_open_for_path};
use crate::core::pool::{Pool, ProgressWatch};
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotifyWait {
    Signaled,
    TimedOut,
    Unavailable,
}

enum Source {
    Semaphore(crate::core::notify::PoolSemaphore),
    Progress { watch: ProgressWatch, observed: u64 },
}

pub struct NotifyHandle {
    inner: Source,
}

impl NotifyHandle {
    pub fn wait(&mut self, timeout: Duration) -> NotifyWait {
        let observed = match &self.inner {
            Source::Progress { observed, .. } => *observed,
            Source::Semaphore(_) => 0,
        };
        match self.wait_for_generation(observed, timeout) {
            Ok(WaitOutcome::Signaled) => NotifyWait::Signaled,
            Ok(WaitOutcome::TimedOut) => NotifyWait::TimedOut,
            Err(_) => NotifyWait::Unavailable,
        }
    }

    pub(crate) fn wait_for_generation(
        &mut self,
        expected: u64,
        timeout: Duration,
    ) -> Result<WaitOutcome, NotifyError> {
        match &mut self.inner {
            Source::Semaphore(semaphore) => semaphore.wait(timeout),
            Source::Progress { watch, observed } => {
                let result = watch.wait(expected, timeout);
                *observed = watch.generation();
                result
            }
        }
    }
}

/// Register hints for this opened pool without reopening its path.
/// A notification owns its header mapping and remains valid after Pool drop.
pub fn open_for_pool(pool: &Pool) -> Option<NotifyHandle> {
    if crate::core::notify::shared_address_available() {
        let watch = pool.progress_watch().ok()?;
        let observed = watch.generation();
        Some(NotifyHandle {
            inner: Source::Progress { watch, observed },
        })
    } else {
        core_open_for_path(pool.path())
            .ok()
            .map(|inner| NotifyHandle {
                inner: Source::Semaphore(inner),
            })
    }
}

/// Some backends require an existing openable pool. Poll if hints are unavailable.
pub fn open_for_path(path: &Path) -> Option<NotifyHandle> {
    if crate::core::notify::shared_address_available() {
        open_for_pool(&Pool::open(path).ok()?)
    } else {
        core_open_for_path(path).ok().map(|inner| NotifyHandle {
            inner: Source::Semaphore(inner),
        })
    }
}

pub fn wait_for_path(path: &Path, timeout: Duration) -> NotifyWait {
    let Some(mut handle) = open_for_path(path) else {
        return NotifyWait::Unavailable;
    };
    handle.wait(timeout)
}
