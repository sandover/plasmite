//! Purpose: Provide best-effort per-pool notifications via shared-address or semaphore waits.
//! Exports: `PoolSemaphore`, `NotifyError`, `WaitOutcome`, `pool_semaphore_name`, `post_for_name`.
//! Role: Optimization for tail-style consumers; correctness must not depend on notify.
//! Invariants: Name derivation is deterministic; failures never panic or block progress.
//! Invariants: Unsupported semaphore operations surface as `NotifyError::Unavailable`.

use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicU64;
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

pub(crate) trait SemaphoreBackend: Clone {
    type Handle;

    fn open(&self, name: &str) -> Result<Self::Handle, NotifyError>;
    fn post(&self, handle: &Self::Handle) -> Result<(), NotifyError>;
    fn wait(&self, handle: &Self::Handle, timeout: Duration) -> Result<WaitOutcome, NotifyError>;
    fn close(&self, handle: &Self::Handle);
}

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
            return Err(map_notify_error());
        }
        Ok(handle)
    }

    fn post(&self, handle: &Self::Handle) -> Result<(), NotifyError> {
        let rc = unsafe { libc::sem_post(*handle) };
        if rc != 0 {
            return Err(map_notify_error());
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
                _ => return Err(map_notify_error_with(err)),
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

// SAFETY: Each wrapper owns one sem_open reference and cannot clone it.
// POSIX semaphore operations permit concurrent threads; Rust references keep
// the handle alive until all operations finish and its owner closes it.
#[cfg(unix)]
unsafe impl Send for PoolSemaphore {}
#[cfg(unix)]
unsafe impl Sync for PoolSemaphore {}

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
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

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

pub(crate) fn pool_semaphore_name(path: &Path) -> String {
    let bytes = canonical_path_bytes(path);
    let digest = Sha256::digest(&bytes);
    // Darwin permits at most 30 name bytes plus the terminating NUL. Leave
    // room for the leading slash and prefix; 96 hash bits distinguish pools.
    let mut hex = String::with_capacity(24);
    for byte in &digest[..12] {
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

pub(crate) fn post_for_name(
    name: &str,
    cached: &mut Option<PoolSemaphore>,
) -> Result<(), NotifyError> {
    #[cfg(test)]
    if FORCE_UNAVAILABLE.load(Ordering::SeqCst) {
        return Err(NotifyError::Unavailable);
    }
    if cached.is_none() {
        *cached = Some(PoolSemaphore::open_with_backend(
            name.to_owned(),
            OsSemaphoreBackend,
        )?);
    }
    let result = cached.as_ref().expect("opened semaphore").post();
    if result.is_err() {
        // Retry opening on the next append; committed storage remains valid.
        *cached = None;
    }
    result
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
    fn semaphore_names_are_stable_and_distinct_for_existing_and_missing_pools() {
        let temp = tempfile::tempdir().expect("tempdir");
        let existing = temp.path().join("first.plasmite");
        let other_existing = temp.path().join("second.plasmite");
        std::fs::write(&existing, []).expect("first pool path");
        std::fs::write(&other_existing, []).expect("second pool path");
        for (first_path, second_path) in [
            (existing.clone(), other_existing),
            (
                temp.path().join("missing-first"),
                temp.path().join("missing-second"),
            ),
        ] {
            let first = pool_semaphore_name(&first_path);
            assert_eq!(first, pool_semaphore_name(&first_path));
            assert_ne!(first, pool_semaphore_name(&second_path));
            assert!(first.starts_with("plsm-"));
            assert!(first.len() + 1 < 31, "include the leading slash");
        }
        assert_eq!(
            pool_semaphore_name(&existing),
            pool_semaphore_name(&temp.path().join(".").join("first.plasmite"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_handles_share_signals_with_a_platform_sized_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let name = pool_semaphore_name(&temp.path().join("native.plasmite"));
        struct Unlink(CString);
        impl Drop for Unlink {
            fn drop(&mut self) {
                unsafe { libc::sem_unlink(self.0.as_ptr()) };
            }
        }
        let _cleanup = Unlink(CString::new(format!("/{name}")).unwrap());
        // Exercise the real backend directly: unavailable notifications must
        // fail this test rather than silently turn it into a polling test.
        let receiver = Semaphore::open_with_backend(name.clone(), OsSemaphoreBackend).unwrap();
        assert_eq!(
            receiver.wait(Duration::ZERO).unwrap(),
            WaitOutcome::TimedOut
        );
        let mut cached = None;
        post_for_name(&name, &mut cached).unwrap();
        post_for_name(&name, &mut cached).unwrap();
        assert_eq!(
            receiver.wait(Duration::from_secs(1)).unwrap(),
            WaitOutcome::Signaled
        );
        assert_eq!(
            receiver.wait(Duration::ZERO).unwrap(),
            WaitOutcome::Signaled
        );
        assert_eq!(
            receiver.wait(Duration::ZERO).unwrap(),
            WaitOutcome::TimedOut
        );
        // Closing one writer reference leaves the independent receiver valid.
        drop(cached.take());
        post_for_name(&name, &mut cached).unwrap();
        // Moving the sole owner across threads also closes it exactly once.
        std::thread::spawn(move || cached.unwrap().post().unwrap())
            .join()
            .unwrap();
        for _ in 0..2 {
            assert_eq!(
                receiver.wait(Duration::ZERO).unwrap(),
                WaitOutcome::Signaled
            );
        }
        assert_eq!(
            receiver.wait(Duration::ZERO).unwrap(),
            WaitOutcome::TimedOut
        );
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
