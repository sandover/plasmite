//! Purpose: Provide best-effort per-pool notifications via named semaphores.
//! Exports: `PoolSemaphore`, `NotifyError`, `WaitOutcome`, `pool_semaphore_name`, `post_for_path`.
//! Role: Optimization for tail-style consumers; correctness must not depend on notify.
//! Invariants: Name derivation is deterministic; failures never panic or block progress.
//! Invariants: Unsupported semaphore operations surface as `NotifyError::Unavailable`.

use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WaitOutcome {
    Signaled,
    TimedOut,
}

#[derive(Debug)]
pub(crate) enum NotifyError {
    Unavailable,
    Io(io::Error),
}

pub(crate) trait SemaphoreBackend: Clone {
    type Handle;

    fn open(&self, name: &str) -> Result<Self::Handle, NotifyError>;
    fn post(&self, handle: &Self::Handle) -> Result<(), NotifyError>;
    fn wait(&self, handle: &Self::Handle, timeout: Duration) -> Result<WaitOutcome, NotifyError>;
    fn close(&self, handle: &Self::Handle);
}

#[derive(Clone)]
pub(crate) struct Semaphore<B: SemaphoreBackend> {
    handle: B::Handle,
    backend: B,
}

impl<B: SemaphoreBackend> Semaphore<B> {
    fn open_with_backend(name: String, backend: B) -> Result<Self, NotifyError> {
        let handle = backend.open(&name)?;
        Ok(Self { handle, backend })
    }

    pub(crate) fn post(&self) -> Result<(), NotifyError> {
        self.backend.post(&self.handle)
    }

    pub(crate) fn wait(&self, timeout: Duration) -> Result<WaitOutcome, NotifyError> {
        self.backend.wait(&self.handle, timeout)
    }
}

impl<B: SemaphoreBackend> Drop for Semaphore<B> {
    fn drop(&mut self) {
        self.backend.close(&self.handle);
    }
}

#[derive(Clone)]
pub(crate) struct OsSemaphoreBackend;

#[cfg(unix)]
impl SemaphoreBackend for OsSemaphoreBackend {
    type Handle = *mut libc::sem_t;

    fn open(&self, name: &str) -> Result<Self::Handle, NotifyError> {
        let full = format!("/{name}");
        let c_name = CString::new(full).map_err(|_| NotifyError::Unavailable)?;
        let mode = (libc::S_IRUSR | libc::S_IWUSR) as libc::mode_t;
        let handle =
            unsafe { libc::sem_open(c_name.as_ptr(), libc::O_CREAT, mode as libc::c_uint, 0) };
        if handle == libc::SEM_FAILED {
            return Err(map_sem_error());
        }
        Ok(handle)
    }

    fn post(&self, handle: &Self::Handle) -> Result<(), NotifyError> {
        let rc = unsafe { libc::sem_post(*handle) };
        if rc != 0 {
            return Err(map_sem_error());
        }
        Ok(())
    }

    fn wait(&self, handle: &Self::Handle, timeout: Duration) -> Result<WaitOutcome, NotifyError> {
        let start = SystemTime::now();
        let poll = Duration::from_millis(5).min(timeout.max(Duration::from_millis(1)));

        loop {
            let rc = unsafe { libc::sem_trywait(*handle) };
            if rc == 0 {
                return Ok(WaitOutcome::Signaled);
            }
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(code) if code == libc::EAGAIN => {
                    let elapsed = start.elapsed().unwrap_or_default();
                    if elapsed >= timeout {
                        return Ok(WaitOutcome::TimedOut);
                    }
                    std::thread::sleep(poll);
                }
                Some(code) if code == libc::EINTR => continue,
                _ => return Err(map_sem_error_with(err)),
            }
        }
    }

    fn close(&self, handle: &Self::Handle) {
        unsafe {
            libc::sem_close(*handle);
        }
    }
}

#[cfg(not(unix))]
impl SemaphoreBackend for OsSemaphoreBackend {
    type Handle = ();

    fn open(&self, _name: &str) -> Result<Self::Handle, NotifyError> {
        Err(NotifyError::Unavailable)
    }

    fn post(&self, _handle: &Self::Handle) -> Result<(), NotifyError> {
        Err(NotifyError::Unavailable)
    }

    fn wait(&self, _handle: &Self::Handle, _timeout: Duration) -> Result<WaitOutcome, NotifyError> {
        Err(NotifyError::Unavailable)
    }

    fn close(&self, _handle: &Self::Handle) {}
}

pub(crate) type PoolSemaphore = Semaphore<OsSemaphoreBackend>;

pub(crate) fn pool_semaphore_name(path: &Path) -> String {
    let bytes = canonical_path_bytes(path);
    let digest = Sha256::digest(&bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    format!("plsm-{hex}")
}

pub(crate) fn open_for_path(path: &Path) -> Result<PoolSemaphore, NotifyError> {
    #[cfg(test)]
    if FORCE_UNAVAILABLE.load(Ordering::SeqCst) {
        return Err(NotifyError::Unavailable);
    }
    let name = pool_semaphore_name(path);
    PoolSemaphore::open_with_backend(name, OsSemaphoreBackend)
}

pub(crate) fn post_for_path(path: &Path) -> Result<(), NotifyError> {
    #[cfg(test)]
    if FORCE_UNAVAILABLE.load(Ordering::SeqCst) {
        return Err(NotifyError::Unavailable);
    }
    let semaphore = open_for_path(path)?;
    semaphore.post()
}

#[cfg(test)]
pub(crate) fn force_unavailable_for_tests(enabled: bool) {
    FORCE_UNAVAILABLE.store(enabled, Ordering::SeqCst);
}

#[cfg(test)]
static FORCE_UNAVAILABLE: AtomicBool = AtomicBool::new(false);

fn canonical_path_bytes(path: &Path) -> Vec<u8> {
    let resolved = std::fs::canonicalize(path);
    let path = resolved.as_ref().map_or(path, |value| value.as_path());
    #[cfg(unix)]
    {
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().as_bytes().to_vec()
    }
}

#[cfg(unix)]
fn map_sem_error() -> NotifyError {
    map_sem_error_with(io::Error::last_os_error())
}

#[cfg(unix)]
fn map_sem_error_with(err: io::Error) -> NotifyError {
    match err.raw_os_error() {
        Some(code) if code == libc::ENOSYS || code == libc::ENOTSUP => NotifyError::Unavailable,
        _ => NotifyError::Io(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    #[derive(Clone, Default)]
    struct TestBackend {
        semaphores: Arc<Mutex<HashMap<String, Arc<TestSemaphoreState>>>>,
    }

    struct TestSemaphoreState {
        count: Mutex<u64>,
        ready: Condvar,
    }

    impl TestSemaphoreState {
        fn new() -> Self {
            Self {
                count: Mutex::new(0),
                ready: Condvar::new(),
            }
        }
    }

    impl SemaphoreBackend for TestBackend {
        type Handle = Arc<TestSemaphoreState>;

        fn open(&self, name: &str) -> Result<Self::Handle, NotifyError> {
            let mut guard = self.semaphores.lock().expect("lock");
            Ok(guard
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(TestSemaphoreState::new()))
                .clone())
        }

        fn post(&self, handle: &Self::Handle) -> Result<(), NotifyError> {
            let mut count = handle.count.lock().expect("lock");
            *count += 1;
            handle.ready.notify_all();
            Ok(())
        }

        fn wait(
            &self,
            handle: &Self::Handle,
            timeout: Duration,
        ) -> Result<WaitOutcome, NotifyError> {
            let mut count = handle.count.lock().expect("lock");
            if *count > 0 {
                *count -= 1;
                return Ok(WaitOutcome::Signaled);
            }

            let (mut count, result) = handle.ready.wait_timeout(count, timeout).expect("wait");
            if *count > 0 {
                *count -= 1;
                return Ok(WaitOutcome::Signaled);
            }
            if result.timed_out() {
                return Ok(WaitOutcome::TimedOut);
            }
            Ok(WaitOutcome::TimedOut)
        }

        fn close(&self, _handle: &Self::Handle) {}
    }

    #[test]
    fn semaphore_name_is_stable() {
        let path = Path::new(".scratch/pools/test.plasmite");
        let first = pool_semaphore_name(path);
        let second = pool_semaphore_name(path);
        assert_eq!(first, second);
        assert!(first.starts_with("plsm-"));
    }

    #[test]
    fn semaphore_name_fallback_is_stable() {
        let path = Path::new("does-not-exist.plasmite");
        let first = pool_semaphore_name(path);
        let second = pool_semaphore_name(path);
        assert_eq!(first, second);
    }

    #[test]
    fn test_backend_post_and_wait() {
        let backend = TestBackend::default();
        let name = pool_semaphore_name(Path::new("test.plasmite"));
        let sem_a = Semaphore::open_with_backend(name.clone(), backend.clone()).expect("open");
        let sem_b = Semaphore::open_with_backend(name, backend).expect("open");

        assert_eq!(
            sem_a.wait(Duration::from_millis(5)).expect("wait"),
            WaitOutcome::TimedOut
        );
        sem_b.post().expect("post");
        assert_eq!(
            sem_a.wait(Duration::from_millis(50)).expect("wait"),
            WaitOutcome::Signaled
        );
    }
}
