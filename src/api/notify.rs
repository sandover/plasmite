//! Purpose: Expose best-effort notification helpers for external callers.
//! Exports: `NotifyWait`, `wait_for_path`.
//! Role: Thin wrapper over core notify for CLI/bindings use.
//! Invariants: Wait results are best-effort; callers must fall back to polling.
//! Invariants: Unavailable notify never blocks progress.

#[cfg(not(any(target_os = "linux", windows)))]
use crate::core::notify::open_for_path as core_open_for_path;
use crate::core::notify::{NotifyError, WaitOutcome};
use crate::core::pool::{Pool, ProgressWatch};
use std::path::Path;
use std::time::Duration;

mod shared;
#[doc(hidden)]
pub use shared::{PoolSubscription, SharedPoolWaiters};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotifyWait {
    Signaled,
    TimedOut,
    Unavailable,
}

enum Source {
    #[cfg(not(any(target_os = "linux", windows)))]
    Semaphore(crate::core::notify::PoolSemaphore),
    Progress {
        watch: ProgressWatch,
        observed: u64,
    },
}

pub struct NotifyHandle {
    inner: Source,
}

impl NotifyHandle {
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    pub(crate) fn uses_progress(&self) -> bool {
        matches!(self.inner, Source::Progress { .. })
    }

    pub fn wait(&mut self, timeout: Duration) -> NotifyWait {
        let observed = match &self.inner {
            Source::Progress { observed, .. } => *observed,
            #[cfg(not(any(target_os = "linux", windows)))]
            Source::Semaphore(_) => 0,
        };
        match self.wait_for_generation(
            observed,
            timeout,
            #[cfg(any(target_os = "linux", windows))]
            None,
        ) {
            Ok(WaitOutcome::Signaled) => NotifyWait::Signaled,
            Ok(WaitOutcome::TimedOut) => NotifyWait::TimedOut,
            Err(_) => NotifyWait::Unavailable,
        }
    }

    pub(crate) fn wait_for_generation(
        &mut self,
        expected: u64,
        timeout: Duration,
        #[cfg(any(target_os = "linux", windows))] cancel: Option<
            &crate::core::notify::CancelSignal,
        >,
    ) -> Result<WaitOutcome, NotifyError> {
        match &mut self.inner {
            #[cfg(not(any(target_os = "linux", windows)))]
            Source::Semaphore(semaphore) => semaphore.wait(timeout),
            Source::Progress { watch, observed } => {
                let result = watch.wait(
                    expected,
                    timeout,
                    #[cfg(any(target_os = "linux", windows))]
                    cancel,
                );
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
        #[cfg(any(target_os = "linux", windows))]
        return None;
        #[cfg(not(any(target_os = "linux", windows)))]
        core_open_for_path(pool.path())
            .ok()
            .map(|inner| NotifyHandle {
                inner: Source::Semaphore(inner),
            })
    }
}

/// Some backends require an existing openable pool. Poll if hints are unavailable.
pub fn open_for_path(path: &Path) -> Option<NotifyHandle> {
    #[cfg(any(target_os = "linux", windows))]
    return open_for_pool(&Pool::open(path).ok()?);
    #[cfg(not(any(target_os = "linux", windows)))]
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
