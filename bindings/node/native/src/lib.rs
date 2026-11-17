/*
Purpose: Provide a Node N-API binding over the libplasmite C ABI.
Key Exports: Client, Pool, Stream, Durability, ErrorKind.
Role: Official Node/TypeScript binding that mirrors the v0 API contract.
Invariants: Calls into C ABI only; JSON bytes in/out; explicit Close methods.
Invariants: Errors carry stable kinds and context as own JavaScript properties.
Notes: The addon links against libplasmite and does not re-implement internals.
*/

use libc::{c_char, c_int};
use napi::bindgen_prelude::{AsyncTask, BigInt, Buffer, Either, Status};
use napi::{Env, Error, JsBuffer, NapiValue, Result, Task};
use napi_derive::napi;
use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::Arc;
mod completion;
mod input;
mod views;
mod waits;
pub use views::native_view_from_bytes;

#[repr(C)]
struct plsm_client_t {
    _private: [u8; 0],
}

#[repr(C)]
struct plsm_pool_t {
    _private: [u8; 0],
}

#[repr(C)]
struct plsm_stream_t {
    _private: [u8; 0],
}

#[repr(C)]
struct plsm_lite3_stream_t {
    _private: [u8; 0],
}

#[repr(C)]
struct plsm_udp_client_t {
    _private: [u8; 0],
}

#[repr(C)]
struct plsm_udp_pool_t {
    _private: [u8; 0],
}

#[repr(C)]
struct plsm_udp_follow_t {
    _private: [u8; 0],
}

#[repr(C)]
struct plsm_buf_t {
    data: *mut u8,
    len: usize,
}

#[repr(C)]
#[derive(Default)]
struct plsm_append_identity_t {
    seq: u64,
    timestamp_ns: u64,
}

#[repr(C)]
struct plsm_udp_event_t {
    kind: u32,
    reason: u32,
    seq: u64,
    timestamp_ns: u64,
    first: u64,
    last: u64,
    start_seq: u64,
    next_seq: u64,
    end_seq: u64,
    has_end: u8,
    terminal: u8,
    retention_gap: u8,
    reserved: u8,
    payload: plsm_buf_t,
}

#[repr(C)]
struct plsm_lite3_frame_t {
    seq: u64,
    timestamp_ns: u64,
    flags: u32,
    payload: plsm_buf_t,
}

#[repr(C)]
struct plsm_error_t {
    kind: i32,
    message: *mut c_char,
    path: *mut c_char,
    seq: u64,
    offset: u64,
    has_seq: u8,
    has_offset: u8,
}

#[repr(C)]
struct plsm_follow_options_t {
    struct_size: u32,
    gap_policy: u32,
    since_seq: u64,
    max_messages: u64,
    timeout_ms: u64,
    has_since: u32,
    has_max: u32,
    has_timeout: u32,
    reserved: u32,
}

#[repr(C)]
struct TextSpan {
    data: *const u8,
    len: usize,
}
#[cfg(unix)]
fn stream_views_available(udp: bool) -> bool {
    static AVAILABLE: std::sync::OnceLock<(bool, bool)> = std::sync::OnceLock::new();
    let flags = AVAILABLE.get_or_init(|| {
        let has = |names: &[&CStr]| {
            views::available()
                && names.iter().all(|name| {
                    !unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) }.is_null()
                })
        };
        (
            has(&[c"plsm_pool_follow_views", c"plsm_view_stream_next"]),
            has(&[c"plsm_udp_pool_follow_views", c"plsm_udp_follow_next_view"]),
        )
    });
    if udp { flags.1 } else { flags.0 }
}
#[cfg(not(unix))]
fn stream_views_available(_: bool) -> bool {
    views::available()
}

unsafe extern "C" {
    fn plsm_pool_follow_views(
        pool: *mut plsm_pool_t,
        options: *const plsm_follow_options_t,
        tags: *const TextSpan,
        count: usize,
        out: *mut *mut plsm_stream_t,
        error: *mut *mut plsm_error_t,
    ) -> i32;
    fn plsm_view_stream_next(
        stream: *mut plsm_stream_t,
        timeout: u64,
        max_count: usize,
        max_bytes: usize,
        out: *mut *const views::Owner,
        count: *mut usize,
        error: *mut *mut plsm_error_t,
    ) -> i32;
    fn plsm_udp_pool_follow_views(
        pool: *mut plsm_udp_pool_t,
        start_kind: u32,
        start_value: u64,
        live: u32,
        out: *mut *mut plsm_udp_follow_t,
        error: *mut *mut plsm_error_t,
    ) -> i32;
    fn plsm_udp_follow_next_view(
        follow: *mut plsm_udp_follow_t,
        timeout: u64,
        event: *mut plsm_udp_event_t,
        owner: *mut *const views::Owner,
        error: *mut *mut plsm_error_t,
    ) -> i32;

    fn plsm_client_new(
        pool_dir: *const c_char,
        out_client: *mut *mut plsm_client_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_client_free(client: *mut plsm_client_t);

    fn plsm_pool_create(
        client: *mut plsm_client_t,
        pool_ref: *const c_char,
        size_bytes: u64,
        out_pool: *mut *mut plsm_pool_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_pool_open(
        client: *mut plsm_client_t,
        pool_ref: *const c_char,
        out_pool: *mut *mut plsm_pool_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_pool_free(pool: *mut plsm_pool_t);

    fn plsm_pool_append_json(
        pool: *mut plsm_pool_t,
        json_bytes: *const u8,
        json_len: usize,
        tags: *const *const c_char,
        tags_len: usize,
        durability: u32,
        out_message: *mut plsm_buf_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;

    fn plsm_pool_append_lite3(
        pool: *mut plsm_pool_t,
        payload: *const u8,
        payload_len: usize,
        durability: u32,
        out_seq: *mut u64,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;

    fn plsm_pool_append_json_identity(
        pool: *mut plsm_pool_t,
        payload: *const u8,
        len: usize,
        tags: *const *const c_char,
        tags_len: usize,
        durability: u32,
        out: *mut plsm_append_identity_t,
        err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_pool_append_lite3_identity(
        pool: *mut plsm_pool_t,
        payload: *const u8,
        len: usize,
        durability: u32,
        out: *mut plsm_append_identity_t,
        err: *mut *mut plsm_error_t,
    ) -> c_int;

    fn plsm_pool_get_json(
        pool: *mut plsm_pool_t,
        seq: u64,
        out_message: *mut plsm_buf_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;

    fn plsm_pool_get_lite3(
        pool: *mut plsm_pool_t,
        seq: u64,
        out_frame: *mut plsm_lite3_frame_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;

    fn plsm_pool_follow(pool: *mut plsm_pool_t, options: *const plsm_follow_options_t, out: *mut *mut plsm_stream_t, error: *mut *mut plsm_error_t) -> i32;
    fn plsm_pool_follow_lite3(pool: *mut plsm_pool_t, options: *const plsm_follow_options_t, out: *mut *mut plsm_lite3_stream_t, error: *mut *mut plsm_error_t) -> i32;
    fn plsm_lite3_stream_next(
        stream: *mut plsm_lite3_stream_t,
        out_frame: *mut plsm_lite3_frame_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;

    fn plsm_stream_next_wait(
        stream: *mut plsm_stream_t,
        timeout_ms: u64,
        out: *mut plsm_buf_t,
        err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_lite3_stream_next_wait(
        stream: *mut plsm_lite3_stream_t,
        timeout_ms: u64,
        out: *mut plsm_lite3_frame_t,
        err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_stream_cancel(stream: *mut plsm_stream_t);
    fn plsm_lite3_stream_cancel(stream: *mut plsm_lite3_stream_t);
    fn plsm_stream_next(stream: *mut plsm_stream_t, out: *mut plsm_buf_t, error: *mut *mut plsm_error_t) -> i32;
    fn plsm_stream_free(stream: *mut plsm_stream_t);
    fn plsm_lite3_stream_free(stream: *mut plsm_lite3_stream_t);

    fn plsm_buf_free(buf: *mut plsm_buf_t);
    fn plsm_error_free(err: *mut plsm_error_t);

    fn plsm_udp_client_new(
        endpoint: *const c_char,
        out_client: *mut *mut plsm_udp_client_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_udp_client_free(client: *mut plsm_udp_client_t);
    fn plsm_udp_client_open_pool(
        client: *mut plsm_udp_client_t,
        pool_name: *const c_char,
        out_pool: *mut *mut plsm_udp_pool_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_udp_pool_free(pool: *mut plsm_udp_pool_t);
    fn plsm_udp_pool_send_json(
        pool: *mut plsm_udp_pool_t,
        json_bytes: *const u8,
        json_len: usize,
        tags: *const *const c_char,
        tags_len: usize,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_udp_pool_send_lite3(
        pool: *mut plsm_udp_pool_t,
        payload: *const u8,
        payload_len: usize,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_udp_pool_follow(
        pool: *mut plsm_udp_pool_t,
        start_kind: u32,
        start_value: u64,
        live: u32,
        out_follow: *mut *mut plsm_udp_follow_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_udp_follow_next(
        follow: *mut plsm_udp_follow_t,
        timeout_ms: u64,
        out_event: *mut plsm_udp_event_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_udp_follow_cancel(follow: *mut plsm_udp_follow_t);
    fn plsm_udp_follow_close(follow: *mut plsm_udp_follow_t);
    fn plsm_udp_event_free(event: *mut plsm_udp_event_t);
    fn plsm_udp_event_message(
        event: *const plsm_udp_event_t,
        out_message: *mut plsm_buf_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;
}

#[napi]
#[derive(Debug, PartialEq, Eq)]
pub enum Durability {
    Fast = 0,
    Flush = 1,
}

#[napi]
#[derive(Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Internal = 1,
    Usage = 2,
    NotFound = 3,
    AlreadyExists = 4,
    Busy = 5,
    Permission = 6,
    Corrupt = 7,
    Io = 8,
    RetentionGap = 9,
}

#[napi]
pub struct Client {
    ptr: *mut plsm_client_t,
}

#[napi]
impl Client {
    #[napi(constructor)]
    pub fn new(env: Env, pool_dir: String) -> Result<Self> {
        let pool_dir = CString::new(pool_dir)
            .map_err(|_| Error::new(Status::InvalidArg, "pool_dir contains NUL"))?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_client_new(pool_dir.as_ptr(), &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(Self { ptr: out })
    }

    #[napi]
    pub fn create_pool(
        &self,
        env: Env,
        pool_ref: String,
        size_bytes: Either<u32, BigInt>,
    ) -> Result<Pool> {
        let pool_ref = CString::new(pool_ref)
            .map_err(|_| Error::new(Status::InvalidArg, "pool_ref contains NUL"))?;
        let size_bytes = to_u64(size_bytes, "size_bytes")?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = unsafe {
            plsm_pool_create(self.ptr, pool_ref.as_ptr(), size_bytes, &mut out, &mut err)
        };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(Pool {
            handle: Some(Arc::new(PoolHandle {
                ptr: out,
                operation: std::sync::Mutex::new(()),
            })),
        })
    }

    #[napi]
    pub fn open_pool(&self, env: Env, pool_ref: String) -> Result<Pool> {
        let pool_ref = CString::new(pool_ref)
            .map_err(|_| Error::new(Status::InvalidArg, "pool_ref contains NUL"))?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_pool_open(self.ptr, pool_ref.as_ptr(), &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(Pool {
            handle: Some(Arc::new(PoolHandle {
                ptr: out,
                operation: std::sync::Mutex::new(()),
            })),
        })
    }

    #[napi]
    pub fn close(&mut self) {
        if !self.ptr.is_null() {
            unsafe { plsm_client_free(self.ptr) };
            self.ptr = ptr::null_mut();
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}

#[napi]
pub struct Pool {
    handle: Option<Arc<PoolHandle>>,
}
struct PoolHandle {
    ptr: *mut plsm_pool_t,
    operation: std::sync::Mutex<()>,
}
// The mutex serializes every C pool call; the last Arc follows in-flight jobs.
unsafe impl Send for PoolHandle {}
unsafe impl Sync for PoolHandle {}
impl Drop for PoolHandle {
    fn drop(&mut self) {
        unsafe { plsm_pool_free(self.ptr) }
    }
}
fn pool_error(env: Env, kind: &str, message: &str) -> Error {
    let result = (|| -> Result<Error> {
        let mut error = env.create_error(Error::from_reason(message))?;
        error.set_named_property("kind", kind)?;
        Ok(Error::from(error.into_unknown()))
    })();
    result.unwrap_or_else(|error| error)
}

impl Pool {
    fn operation(&self, env: Env) -> Result<std::sync::MutexGuard<'_, ()>> {
        let handle = self
            .handle
            .as_ref()
            .ok_or_else(|| pool_error(env, "Usage", "pool is closed"))?;
        match handle.operation.try_lock() {
            Ok(guard) => Ok(guard),
            Err(_) => Err(pool_error(
                env,
                "Busy",
                "pool has an active asynchronous operation",
            )),
        }
    }
}

#[napi(object)]
pub struct AppendReceipt {
    pub seq: BigInt,
    #[napi(js_name = "timestampNs")]
    pub timestamp_ns: BigInt,
}

impl From<plsm_append_identity_t> for AppendReceipt {
    fn from(identity: plsm_append_identity_t) -> Self {
        Self {
            seq: BigInt::from(identity.seq),
            timestamp_ns: BigInt::from(identity.timestamp_ns),
        }
    }
}

#[napi(object)]
pub struct Lite3Frame {
    pub seq: BigInt,
    #[napi(js_name = "timestampNs")]
    pub timestamp_ns: BigInt,
    pub flags: u32,
    pub payload: JsBuffer,
}

#[napi]
impl Pool {
    #[napi]
    pub fn prepare_append(
        &self,
        env: Env,
        durability: Durability,
    ) -> Result<input::NativeAppendTarget> {
        if !views::available() {
            return Err(pool_error(
                env,
                "Usage",
                "loaded libplasmite lacks owned-view support",
            ));
        }
        let handle = self
            .handle
            .as_ref()
            .cloned()
            .ok_or_else(|| pool_error(env, "Usage", "pool is closed"))?;
        Ok(input::NativeAppendTarget::new(handle, durability as u32))
    }

    #[napi]
    pub fn append_json(
        &self,
        env: Env,
        payload: Buffer,
        tags: Vec<String>,
        durability: Durability,
    ) -> Result<Buffer> {
        let _operation = self.operation(env)?;
        let c_tags = CStringArray::new(&tags)?;
        let mut out = plsm_buf_t {
            data: ptr::null_mut(),
            len: 0,
        };
        let mut err = ptr::null_mut();
        let rc = unsafe {
            plsm_pool_append_json(
                self.handle.as_ref().expect("open pool").ptr,
                payload.as_ptr(),
                payload.len(),
                c_tags.as_ptr(),
                tags.len(),
                durability as u32,
                &mut out,
                &mut err,
            )
        };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(copy_json_buf(out))
    }

    #[napi]
    pub fn append_lite3(
        &self,
        env: Env,
        payload: Buffer,
        durability: Durability,
    ) -> Result<BigInt> {
        let _operation = self.operation(env)?;
        let mut seq = 0u64;
        let mut err = ptr::null_mut();
        let rc = unsafe {
            plsm_pool_append_lite3(
                self.handle.as_ref().expect("open pool").ptr,
                payload.as_ptr(),
                payload.len(),
                durability as u32,
                &mut seq,
                &mut err,
            )
        };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(BigInt::from(seq))
    }

    #[napi]
    pub fn append_json_identity(
        &self,
        env: Env,
        payload: Buffer,
        tags: Vec<String>,
        durability: Durability,
    ) -> Result<AppendReceipt> {
        let _operation = self.operation(env)?;
        let c_tags = CStringArray::new(&tags)?;
        let mut out = plsm_append_identity_t::default();
        let mut err = ptr::null_mut();
        let rc = unsafe {
            plsm_pool_append_json_identity(
                self.handle.as_ref().expect("open pool").ptr,
                payload.as_ptr(),
                payload.len(),
                c_tags.as_ptr(),
                tags.len(),
                durability as u32,
                &mut out,
                &mut err,
            )
        };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(out.into())
    }

    #[napi]
    pub fn append_lite3_identity(
        &self,
        env: Env,
        payload: Buffer,
        durability: Durability,
    ) -> Result<AppendReceipt> {
        let _operation = self.operation(env)?;
        let mut out = plsm_append_identity_t::default();
        let mut err = ptr::null_mut();
        let rc = unsafe {
            plsm_pool_append_lite3_identity(
                self.handle.as_ref().expect("open pool").ptr,
                payload.as_ptr(),
                payload.len(),
                durability as u32,
                &mut out,
                &mut err,
            )
        };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(out.into())
    }

    #[napi]
    pub fn get_json(&self, env: Env, seq: Either<u32, BigInt>) -> Result<Buffer> {
        let _operation = self.operation(env)?;
        let seq = to_u64(seq, "seq")?;
        let mut out = plsm_buf_t {
            data: ptr::null_mut(),
            len: 0,
        };
        let mut err = ptr::null_mut();
        let rc = unsafe {
            plsm_pool_get_json(
                self.handle.as_ref().expect("open pool").ptr,
                seq,
                &mut out,
                &mut err,
            )
        };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(copy_json_buf(out))
    }

    #[napi]
    pub fn get_lite3(&self, env: Env, seq: Either<u32, BigInt>) -> Result<Lite3Frame> {
        let _operation = self.operation(env)?;
        let seq = to_u64(seq, "seq")?;
        let mut out = plsm_lite3_frame_t {
            seq: 0,
            timestamp_ns: 0,
            flags: 0,
            payload: plsm_buf_t {
                data: ptr::null_mut(),
                len: 0,
            },
        };
        let mut err = ptr::null_mut();
        let rc = unsafe {
            plsm_pool_get_lite3(
                self.handle.as_ref().expect("open pool").ptr,
                seq,
                &mut out,
                &mut err,
            )
        };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        take_frame(env, out)
    }

    #[napi]
    pub fn open_follow(
        &self,
        env: Env,
        since_seq: Option<Either<u32, BigInt>>,
        max_messages: Option<Either<u32, BigInt>>,
        timeout_ms: Option<Either<u32, BigInt>>,
        error_on_gap: bool,
        start_kind: u32,
        finite: bool,
    ) -> Result<Stream> {
        let _operation = self.operation(env)?;
        let since_seq = since_seq
            .map(|value| to_u64(value, "since_seq"))
            .transpose()?;
        let max_messages = max_messages
            .map(|value| to_u64(value, "max_messages"))
            .transpose()?;
        let timeout_ms = timeout_ms
            .map(|value| to_u64(value, "timeout_ms"))
            .transpose()?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let options = stream_options(since_seq, max_messages, timeout_ms, error_on_gap, start_kind, finite);
        let rc = unsafe { plsm_pool_follow(self.handle.as_ref().expect("open pool").ptr, &options, &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(Stream {
            state: stream_state(StreamPointer::Json(out)),
        })
    }

    #[napi]
    pub fn open_lite3_follow(
        &self,
        env: Env,
        since_seq: Option<Either<u32, BigInt>>,
        max_messages: Option<Either<u32, BigInt>>,
        timeout_ms: Option<Either<u32, BigInt>>,
        error_on_gap: bool,
        start_kind: u32,
        finite: bool,
    ) -> Result<Lite3Stream> {
        let _operation = self.operation(env)?;
        let since_seq = since_seq
            .map(|value| to_u64(value, "since_seq"))
            .transpose()?;
        let max_messages = max_messages
            .map(|value| to_u64(value, "max_messages"))
            .transpose()?;
        let timeout_ms = timeout_ms
            .map(|value| to_u64(value, "timeout_ms"))
            .transpose()?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let options = stream_options(since_seq, max_messages, timeout_ms, error_on_gap, start_kind, finite);
        let rc = unsafe { plsm_pool_follow_lite3(self.handle.as_ref().expect("open pool").ptr, &options, &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(Lite3Stream {
            state: stream_state(StreamPointer::Lite3(out)),
        })
    }

    #[napi]
    pub fn open_view_follow(
        &self,
        env: Env,
        since_seq: Option<Either<u32, BigInt>>,
        max_messages: Option<Either<u32, BigInt>>,
        timeout_ms: Option<Either<u32, BigInt>>,
        error_on_gap: bool,
        tags: Vec<String>,
        start_kind: u32,
        finite: bool,
    ) -> Result<Stream> {
        if !stream_views_available(false) {
            return Err(pool_error(
                env,
                "Usage",
                "loaded libplasmite lacks owned stream support",
            ));
        }
        let _operation = self.operation(env)?;
        let options = stream_options(
            since_seq.map(|v| to_u64(v, "since_seq")).transpose()?,
            max_messages
                .map(|v| to_u64(v, "max_messages"))
                .transpose()?,
            timeout_ms.map(|v| to_u64(v, "timeout_ms")).transpose()?,
            error_on_gap, start_kind, finite,
        );
        let spans: Vec<_> = tags
            .iter()
            .map(|tag| TextSpan {
                data: tag.as_ptr(),
                len: tag.len(),
            })
            .collect();
        let mut out = ptr::null_mut();
        let mut error = ptr::null_mut();
        if unsafe {
            plsm_pool_follow_views(
                self.handle.as_ref().expect("open pool").ptr,
                &options,
                spans.as_ptr(),
                spans.len(),
                &mut out,
                &mut error,
            )
        } != 0
        {
            return Err(take_error(env, error)?);
        }
        Ok(Stream {
            state: stream_state(StreamPointer::View(out)),
        })
    }

    #[napi]
    pub fn get_view(&self, env: Env, seq: Either<u32, BigInt>) -> Result<AsyncTask<GetViewTask>> {
        if !views::available() {
            return Err(pool_error(
                env,
                "Usage",
                "loaded libplasmite lacks owned-view support; install matching native assets or rebuild libplasmite",
            ));
        }
        Ok(AsyncTask::new(GetViewTask {
            handle: self
                .handle
                .as_ref()
                .cloned()
                .ok_or_else(|| pool_error(env, "Usage", "pool is closed"))?,
            seq: to_u64(seq, "seq")?,
        }))
    }
    #[napi]
    pub fn close(&mut self) {
        self.handle.take();
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.close();
    }
}

// C serializes reader state and accepts concurrent cancellation. Each task owns
// an Arc, so closing a JS wrapper cancels work without freeing an active handle.
#[derive(Clone, Copy)]
enum StreamPointer {
    Json(*mut plsm_stream_t),
    Lite3(*mut plsm_lite3_stream_t),
    Udp(*mut plsm_udp_follow_t),
    UdpView(*mut plsm_udp_follow_t),
    View(*mut plsm_stream_t),
}

struct StreamHandle {
    pointer: StreamPointer,
    reading: std::sync::atomic::AtomicBool,
    cancelled: std::sync::atomic::AtomicBool,
    finished: std::sync::atomic::AtomicBool,
    interrupted: std::sync::atomic::AtomicBool,
}

// SAFETY: C streams and UDP follows permit one reader plus concurrent cancel. ReadLease
// enforces the reader rule; the final Arc frees the handle after active work.
unsafe impl Send for StreamHandle {}
unsafe impl Sync for StreamHandle {}

impl StreamHandle {
    fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        unsafe {
            match self.pointer {
                StreamPointer::Json(ptr) | StreamPointer::View(ptr) => plsm_stream_cancel(ptr),
                StreamPointer::Lite3(ptr) => plsm_lite3_stream_cancel(ptr),
                StreamPointer::Udp(ptr) | StreamPointer::UdpView(ptr) => {
                    plsm_udp_follow_cancel(ptr)
                }
            }
        }
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        unsafe {
            match self.pointer {
                StreamPointer::Json(ptr) | StreamPointer::View(ptr) => plsm_stream_free(ptr),
                StreamPointer::Lite3(ptr) => plsm_lite3_stream_free(ptr),
                StreamPointer::Udp(ptr) | StreamPointer::UdpView(ptr) => plsm_udp_follow_close(ptr),
            }
        }
    }
}

// Node async_hooks can run user code while a Promise/channel is created.
// Keep wrapper mutation behind short locks and hold none across Node-API calls.
struct StreamState {
    handle: Option<Arc<StreamHandle>>,
    channel: Option<Arc<completion::Channel>>,
    worker: Option<waits::Worker>,
}
fn stream_state(pointer: StreamPointer) -> std::sync::Mutex<StreamState> {
    std::sync::Mutex::new(StreamState {
        handle: Some(Arc::new(StreamHandle {
            pointer,
            reading: std::sync::atomic::AtomicBool::new(false),
            cancelled: std::sync::atomic::AtomicBool::new(false),
            finished: std::sync::atomic::AtomicBool::new(false),
            interrupted: std::sync::atomic::AtomicBool::new(false),
        })),
        channel: None,
        worker: None,
    })
}
fn stream_read(state: &std::sync::Mutex<StreamState>) -> Result<ReadLease> {
    ReadLease::new(&state.lock().unwrap().handle)
}
fn close_stream(state: &std::sync::Mutex<StreamState>) {
    let (handle, channel, worker) = {
        let mut state = state.lock().unwrap();
        (
            state.handle.take(),
            state.channel.take(),
            state.worker.take(),
        )
    };
    if let Some(handle) = handle {
        handle.cancel();
    }
    drop(worker);
    drop(channel);
}

struct ReadLease {
    handle: Arc<StreamHandle>,
    active: bool,
}
impl ReadLease {
    fn new(handle: &Option<Arc<StreamHandle>>) -> Result<Self> {
        let handle = handle
            .as_ref()
            .ok_or_else(|| Error::new(Status::GenericFailure, "stream is closed"))?;
        if handle
            .reading
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return Err(Error::new(
                Status::GenericFailure,
                "stream already has an active reader",
            ));
        }
        handle
            .interrupted
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(Self {
            handle: handle.clone(),
            active: true,
        })
    }
}
impl ReadLease {
    fn release(&mut self) {
        if self.active {
            self.handle
                .reading
                .store(false, std::sync::atomic::Ordering::Release);
            self.active = false;
        }
    }
}
impl Drop for ReadLease {
    fn drop(&mut self) {
        self.release();
    }
}

#[napi]
pub struct Stream {
    state: std::sync::Mutex<StreamState>,
}

#[napi]
impl Stream {
    #[napi]
    pub fn next_json(&self, env: Env) -> Result<Option<Buffer>> {
        let read = stream_read(&self.state)?;
        let StreamPointer::Json(ptr) = read.handle.pointer else {
            unreachable!()
        };
        let mut out = plsm_buf_t {
            data: ptr::null_mut(),
            len: 0,
        };
        let mut err = ptr::null_mut();
        match unsafe { plsm_stream_next(ptr, &mut out, &mut err) } {
            1 => Ok(Some(copy_json_buf(out))),
            0 => Ok(None),
            _ => Err(take_error(env, err)?),
        }
    }

    #[napi(ts_return_type = "Promise<StreamWait>")]
    pub fn next_json_wait(
        &self,
        env: Env,
        timeout_ms: Either<u32, BigInt>,
    ) -> Result<napi::JsObject> {
        schedule_stream_read(
            env,
            stream_read(&self.state)?,
            to_u64(timeout_ms, "timeout_ms")?,
            &self.state,
        )
    }

    #[napi]
    pub fn cancel(&self) {
        if let Some(handle) = self.state.lock().unwrap().handle.clone() {
            handle.cancel();
        }
    }

    #[napi]
    pub fn close(&self) {
        close_stream(&self.state);
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.close();
    }
}

#[napi]
pub struct Lite3Stream {
    state: std::sync::Mutex<StreamState>,
}

#[napi]
impl Lite3Stream {
    #[napi]
    pub fn next(&self, env: Env) -> Result<Option<Lite3Frame>> {
        let read = stream_read(&self.state)?;
        let StreamPointer::Lite3(ptr) = read.handle.pointer else {
            unreachable!()
        };
        let mut out = empty_lite3_frame();
        let mut err = ptr::null_mut();
        match unsafe { plsm_lite3_stream_next(ptr, &mut out, &mut err) } {
            1 => take_frame(env, out).map(Some),
            0 => Ok(None),
            _ => Err(take_error(env, err)?),
        }
    }

    #[napi(ts_return_type = "Promise<StreamWait>")]
    pub fn next_wait(&self, env: Env, timeout_ms: Either<u32, BigInt>) -> Result<napi::JsObject> {
        schedule_stream_read(
            env,
            stream_read(&self.state)?,
            to_u64(timeout_ms, "timeout_ms")?,
            &self.state,
        )
    }

    #[napi]
    pub fn cancel(&self) {
        if let Some(handle) = self.state.lock().unwrap().handle.clone() {
            handle.cancel();
        }
    }

    #[napi]
    pub fn close(&self) {
        close_stream(&self.state);
    }
}
impl Drop for Lite3Stream {
    fn drop(&mut self) {
        self.close();
    }
}

fn empty_lite3_frame() -> plsm_lite3_frame_t {
    plsm_lite3_frame_t {
        seq: 0,
        timestamp_ns: 0,
        flags: 0,
        payload: plsm_buf_t {
            data: ptr::null_mut(),
            len: 0,
        },
    }
}

#[napi(object, object_from_js = false)]
pub struct StreamWait {
    pub status: u32,
    pub message: Option<Buffer>,
    pub frame: Option<Lite3Frame>,
    pub event: Option<UdpEvent>,
    pub view: Option<views::NativeView>,
}

pub struct StreamReadTask {
    read: ReadLease,
    timeout_ms: u64,
}

enum StreamPayload {
    Json(Vec<u8>),
    Lite3(NativeBuf),
    Udp(Box<UdpOutput>),
    View(views::Owned),
}

pub struct StreamReadOutput {
    status: i32,
    bytes: Option<StreamPayload>,
    seq: u64,
    timestamp_ns: u64,
    flags: u32,
    error: usize,
}
impl Drop for StreamReadOutput {
    fn drop(&mut self) {
        if self.error != 0 {
            unsafe { plsm_error_free(self.error as *mut plsm_error_t) };
        }
    }
}

// The absolute budget starts before queueing. Idle rotations retain the same
// lease; only completion on the environment thread permits the next read.
fn schedule_stream_read(
    env: Env,
    read: ReadLease,
    timeout_ms: u64,
    state: &std::sync::Mutex<StreamState>,
) -> Result<napi::JsObject> {
    let deadline =
        std::time::Instant::now().checked_add(std::time::Duration::from_millis(timeout_ms));
    let worker = if timeout_ms == 0 {
        None
    } else {
        Some({
            let mut state = state.lock().unwrap();
            if let Some(worker) = &state.worker {
                worker.clone()
            } else {
                let worker = waits::Worker::new()
                    .map_err(|error| Error::new(Status::GenericFailure, error.to_string()))?;
                if state.handle.is_some() {
                    state.worker = Some(worker.clone());
                }
                worker
            }
        })
    };
    let existing = state.lock().unwrap().channel.clone();
    let channel = if let Some(channel) = existing {
        channel
    } else {
        let handle = Arc::downgrade(&read.handle);
        let channel = completion::Channel::new(env, move || {
            if let Some(handle) = handle.upgrade() {
                handle.cancel();
            }
        })?;
        let mut state = state.lock().unwrap();
        if state.handle.is_some() {
            state.channel = Some(Arc::clone(&channel));
        }
        channel
    };
    let (deferred, promise) = channel.promise(env)?;
    let probe = timeout_ms == 0
        || !matches!(
            read.handle.pointer,
            StreamPointer::Udp(_) | StreamPointer::UdpView(_)
        );
    let mut state = Some((deferred, StreamReadTask { read, timeout_ms }));
    waits::submit(
        worker,
        probe,
        Box::new(move |slice_ms| {
            let (_, task) = state.as_mut().expect("pending read");
            let cancelled = task
                .read
                .handle
                .cancelled
                .load(std::sync::atomic::Ordering::Acquire);
            let interrupted = task
                .read
                .handle
                .interrupted
                .load(std::sync::atomic::Ordering::Acquire);
            let remaining =
                deadline.map(|limit| limit.saturating_duration_since(std::time::Instant::now()));
            let expired = timeout_ms != 0 && remaining.is_some_and(|duration| duration.is_zero());
            let output = if cancelled || interrupted || expired {
                Ok(StreamReadOutput {
                    status: if cancelled { 3 } else { 0 },
                    bytes: None,
                    seq: 0,
                    timestamp_ns: 0,
                    flags: 0,
                    error: 0,
                })
            } else {
                task.timeout_ms = remaining.map_or(slice_ms, |duration| {
                    if timeout_ms == 0 {
                        0
                    } else {
                        (duration.as_millis() as u64).max(1).min(slice_ms)
                    }
                });
                task.compute()
            };
            if matches!(&output, Ok(output) if output.status == 0)
                && timeout_ms != 0
                && deadline.is_none_or(|limit| std::time::Instant::now() < limit)
                && !task
                    .read
                    .handle
                    .interrupted
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return false;
            }
            let (deferred, mut task) = state.take().expect("one completion");
            deferred.resolve(move |env| task.resolve(env, output?));
            true
        }),
    );
    Ok(promise)
}

impl Task for StreamReadTask {
    type Output = StreamReadOutput;
    type JsValue = StreamWait;

    fn compute(&mut self) -> Result<Self::Output> {
        if let StreamPointer::Udp(ptr) | StreamPointer::UdpView(ptr) = self.read.handle.pointer {
            return read_udp(
                ptr,
                self.timeout_ms,
                matches!(self.read.handle.pointer, StreamPointer::UdpView(_)),
            );
        }
        if let StreamPointer::View(ptr) = self.read.handle.pointer {
            let mut owner = ptr::null();
            let mut count = 0;
            let mut error = ptr::null_mut();
            let status = unsafe {
                plsm_view_stream_next(
                    ptr,
                    self.timeout_ms,
                    1,
                    usize::MAX,
                    &mut owner,
                    &mut count,
                    &mut error,
                )
            };
            return Ok(StreamReadOutput {
                status,
                bytes: (count == 1).then(|| StreamPayload::View(views::Owned::new(owner))),
                seq: 0,
                timestamp_ns: 0,
                flags: 0,
                error: error as usize,
            });
        }
        let mut frame = empty_lite3_frame();
        let mut err = ptr::null_mut();
        let status = unsafe {
            match self.read.handle.pointer {
                StreamPointer::Json(ptr) => {
                    plsm_stream_next_wait(ptr, self.timeout_ms, &mut frame.payload, &mut err)
                }
                StreamPointer::Lite3(ptr) => {
                    plsm_lite3_stream_next_wait(ptr, self.timeout_ms, &mut frame, &mut err)
                }
                StreamPointer::Udp(_) | StreamPointer::UdpView(_) | StreamPointer::View(_) => {
                    unreachable!()
                }
            }
        };
        let bytes = if status == 1 {
            Some(match self.read.handle.pointer {
                StreamPointer::Json(_) => {
                    StreamPayload::Json(NativeBuf(frame.payload).copy_bytes())
                }
                StreamPointer::Lite3(_) => StreamPayload::Lite3(NativeBuf(frame.payload)),
                StreamPointer::Udp(_) | StreamPointer::UdpView(_) | StreamPointer::View(_) => {
                    unreachable!()
                }
            })
        } else {
            unsafe { plsm_buf_free(&mut frame.payload) };
            None
        };
        let output = StreamReadOutput {
            status,
            bytes,
            seq: frame.seq,
            timestamp_ns: frame.timestamp_ns,
            flags: frame.flags,
            error: err as usize,
        };
        Ok(output)
    }

    fn resolve(&mut self, env: Env, mut output: Self::Output) -> Result<Self::JsValue> {
        // The JS promise may immediately start the next read.
        self.read.release();
        if output.status == 2 || output.status == 3 {
            self.read
                .handle
                .finished
                .store(true, std::sync::atomic::Ordering::Release);
        }
        if output.status < 0 {
            let error = std::mem::replace(&mut output.error, 0);
            return Err(take_error(env, error as *mut plsm_error_t)?);
        }
        let mut result = StreamWait {
            status: output.status as u32,
            message: None,
            frame: None,
            event: None,
            view: None,
        };
        if output.status == 1 {
            match output.bytes.take().expect("item owns payload") {
                StreamPayload::Json(bytes) => result.message = Some(Buffer::from(bytes)),
                StreamPayload::View(owner) => {
                    result.view = Some(views::NativeView::new(env, owner)?)
                }
                StreamPayload::Udp(mut value) => {
                    if let Some(payload) = value.payload.take() {
                        value.event.payload = Some(Buffer::from(payload.copy_bytes()));
                    }
                    if let Some(message) = value.message.take() {
                        value.event.message = Some(Buffer::from(message.copy_bytes()));
                    }
                    result.event = Some(value.event);
                }
                StreamPayload::Lite3(bytes) => {
                    result.frame = Some(Lite3Frame {
                        seq: BigInt::from(output.seq),
                        timestamp_ns: BigInt::from(output.timestamp_ns),
                        flags: output.flags,
                        payload: bytes.into_node(env)?,
                    });
                }
            }
        }
        Ok(result)
    }
}

#[napi]
pub struct UdpClient {
    ptr: *mut plsm_udp_client_t,
}

#[napi]
impl UdpClient {
    #[napi(constructor)]
    pub fn new(env: Env, endpoint: String) -> Result<Self> {
        let endpoint = CString::new(endpoint)
            .map_err(|_| Error::new(Status::InvalidArg, "endpoint contains NUL"))?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_udp_client_new(endpoint.as_ptr(), &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(Self { ptr: out })
    }

    #[napi(js_name = "openPool")]
    pub fn open_pool(&self, env: Env, pool_name: String) -> Result<UdpPool> {
        let pool_name = CString::new(pool_name)
            .map_err(|_| Error::new(Status::InvalidArg, "pool_name contains NUL"))?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc =
            unsafe { plsm_udp_client_open_pool(self.ptr, pool_name.as_ptr(), &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(UdpPool {
            handle: Some(Arc::new(UdpPoolHandle { ptr: out })),
        })
    }

    #[napi]
    pub fn close(&mut self) {
        if !self.ptr.is_null() {
            unsafe { plsm_udp_client_free(self.ptr) };
            self.ptr = ptr::null_mut();
        }
    }
}

impl Drop for UdpClient {
    fn drop(&mut self) {
        self.close();
    }
}

#[napi]
pub struct UdpPool {
    handle: Option<Arc<UdpPoolHandle>>,
}

struct UdpPoolHandle {
    ptr: *mut plsm_udp_pool_t,
}

// SAFETY: The C wrapper owns an immutable Rust UdpPool. Its send and follow
// methods only use shared references, and the Arc keeps the C allocation alive
// through worker tasks even if JavaScript closes or drops its pool wrapper.
unsafe impl Send for UdpPoolHandle {}
unsafe impl Sync for UdpPoolHandle {}

impl Drop for UdpPoolHandle {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { plsm_udp_pool_free(self.ptr) };
            self.ptr = ptr::null_mut();
        }
    }
}

impl UdpPool {
    fn pool_ptr(&self) -> Result<*mut plsm_udp_pool_t> {
        self.handle
            .as_ref()
            .map(|handle| handle.ptr)
            .ok_or_else(|| Error::new(Status::GenericFailure, "UDP pool is closed"))
    }
}

#[napi]
impl UdpPool {
    #[napi(js_name = "sendJson")]
    pub fn send_json(&self, env: Env, payload: Buffer, tags: Vec<String>) -> Result<()> {
        let pool = self.pool_ptr()?;
        let c_tags = CStringArray::new(&tags)?;
        let mut err = ptr::null_mut();
        let rc = unsafe {
            plsm_udp_pool_send_json(
                pool,
                payload.as_ptr(),
                payload.len(),
                c_tags.as_ptr(),
                tags.len(),
                &mut err,
            )
        };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(())
    }

    #[napi(js_name = "sendLite3")]
    pub fn send_lite3(&self, env: Env, payload: Buffer) -> Result<()> {
        let pool = self.pool_ptr()?;
        let mut err = ptr::null_mut();
        let rc =
            unsafe { plsm_udp_pool_send_lite3(pool, payload.as_ptr(), payload.len(), &mut err) };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(())
    }

    #[napi(ts_return_type = "Promise<UdpFollow>")]
    pub fn follow(
        &self,
        env: Env,
        start_kind: u32,
        start_value: Either<u32, BigInt>,
        live: bool,
        owned: Option<bool>,
    ) -> Result<AsyncTask<UdpFollowTask>> {
        let start_value = to_u64(start_value, "start_value")?;
        let owned = owned.unwrap_or(false);
        if owned && !stream_views_available(true) {
            return Err(pool_error(
                env,
                "Usage",
                "loaded libplasmite lacks owned stream support",
            ));
        }
        Ok(AsyncTask::new(UdpFollowTask {
            owned,
            pool: self
                .handle
                .as_ref()
                .ok_or_else(|| Error::new(Status::GenericFailure, "UDP pool is closed"))?
                .clone(),
            start_kind,
            start_value,
            live,
        }))
    }

    #[napi]
    pub fn close(&mut self) {
        self.handle.take();
    }
}

pub struct UdpFollowTask {
    owned: bool,
    pool: Arc<UdpPoolHandle>,
    start_kind: u32,
    start_value: u64,
    live: bool,
}

pub struct UdpFollowOpenResult {
    status: c_int,
    follow: usize,
    error: usize,
}

impl Drop for UdpFollowOpenResult {
    fn drop(&mut self) {
        // Own worker results until resolve transfers them to a JavaScript handle.
        // An abandoned task must release its subscription and error allocation.
        if self.follow != 0 {
            unsafe { plsm_udp_follow_close(self.follow as *mut plsm_udp_follow_t) };
        }
        if self.error != 0 {
            unsafe { plsm_error_free(self.error as *mut plsm_error_t) };
        }
    }
}

impl Task for UdpFollowTask {
    type Output = UdpFollowOpenResult;
    type JsValue = UdpFollow;

    fn compute(&mut self) -> Result<Self::Output> {
        let mut follow = ptr::null_mut();
        let mut err = ptr::null_mut();
        let status = unsafe {
            (if self.owned {
                plsm_udp_pool_follow_views
            } else {
                plsm_udp_pool_follow
            })(
                self.pool.ptr,
                self.start_kind,
                self.start_value,
                u32::from(self.live),
                &mut follow,
                &mut err,
            )
        };
        Ok(UdpFollowOpenResult {
            status,
            follow: follow as usize,
            error: err as usize,
        })
    }

    fn resolve(&mut self, env: Env, mut output: Self::Output) -> Result<Self::JsValue> {
        if output.status != 0 {
            let error = std::mem::replace(&mut output.error, 0);
            return Err(take_error(env, error as *mut plsm_error_t)?);
        }
        let follow = std::mem::replace(&mut output.follow, 0);
        Ok(UdpFollow {
            state: stream_state(if self.owned {
                StreamPointer::UdpView(follow as *mut plsm_udp_follow_t)
            } else {
                StreamPointer::Udp(follow as *mut plsm_udp_follow_t)
            }),
        })
    }
}

impl Drop for UdpPool {
    fn drop(&mut self) {
        self.close();
    }
}

#[napi(object)]
pub struct UdpEvent {
    pub kind: String,
    pub seq: Option<BigInt>,
    #[napi(js_name = "timestampNs")]
    pub timestamp_ns: Option<BigInt>,
    pub payload: Option<Buffer>,
    pub message: Option<Buffer>,
    pub first: Option<BigInt>,
    pub last: Option<BigInt>,
    pub reason: Option<String>,
    #[napi(js_name = "startSeq")]
    pub start_seq: Option<BigInt>,
    #[napi(js_name = "nextSeq")]
    pub next_seq: Option<BigInt>,
    #[napi(js_name = "endSeq")]
    pub end_seq: Option<BigInt>,
    pub terminal: Option<bool>,
    #[napi(js_name = "retentionGap")]
    pub retention_gap: Option<bool>,
}

#[napi(object)]
pub struct UdpWait {
    /// 0=timeout, 1=event, 2=finished, 3=cancelled.
    pub status: u32,
    pub event: Option<UdpEvent>,
}

#[napi]
pub struct UdpFollow {
    state: std::sync::Mutex<StreamState>,
}

#[napi]
impl UdpFollow {
    #[napi(js_name = "nextEvent")]
    pub fn next_event(&self, env: Env, timeout_ms: Either<u32, BigInt>) -> Result<StreamWait> {
        if self.is_finished() {
            return Ok(StreamWait {
                status: 2,
                event: None,
                view: None,
                message: None,
                frame: None,
            });
        }
        let mut task = StreamReadTask {
            read: stream_read(&self.state)?,
            timeout_ms: to_u64(timeout_ms, "timeout_ms")?,
        };
        let output = task.compute()?;
        task.resolve(env, output)
    }

    #[napi(js_name = "nextEventWait", ts_return_type = "Promise<StreamWait>")]
    pub fn next_event_wait(
        &self,
        env: Env,
        timeout_ms: Either<u32, BigInt>,
    ) -> Result<napi::JsObject> {
        schedule_stream_read(
            env,
            stream_read(&self.state)?,
            to_u64(timeout_ms, "timeout_ms")?,
            &self.state,
        )
    }

    #[napi]
    pub fn interrupt_read(&self) {
        if let Some(handle) = self.state.lock().unwrap().handle.clone() {
            handle
                .interrupted
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }

    #[napi(getter, js_name = "isFinished")]
    pub fn is_finished(&self) -> bool {
        self.state
            .lock()
            .unwrap()
            .handle
            .as_ref()
            .is_none_or(|handle| handle.finished.load(std::sync::atomic::Ordering::Acquire))
    }

    #[napi]
    pub fn cancel(&self) {
        if let Some(handle) = self.state.lock().unwrap().handle.clone() {
            handle.cancel();
        }
    }
    #[napi]
    pub fn close(&self) {
        close_stream(&self.state);
    }
}
impl Drop for UdpFollow {
    fn drop(&mut self) {
        self.close();
    }
}

struct UdpOutput {
    event: UdpEvent,
    payload: Option<NativeBuf>,
    message: Option<NativeBuf>,
}

fn read_udp(ptr: *mut plsm_udp_follow_t, timeout: u64, owned: bool) -> Result<StreamReadOutput> {
    let mut event: plsm_udp_event_t = unsafe { std::mem::zeroed() };
    let mut error = ptr::null_mut();
    let mut owner = ptr::null();
    let mut status = unsafe {
        if owned {
            plsm_udp_follow_next_view(ptr, timeout, &mut event, &mut owner, &mut error)
        } else {
            plsm_udp_follow_next(ptr, timeout, &mut event, &mut error)
        }
    };
    let mut message = plsm_buf_t {
        data: ptr::null_mut(),
        len: 0,
    };
    if !owned
        && status == 1
        && event.kind == 1
        && unsafe { plsm_udp_event_message(&event, &mut message, &mut error) } != 0
    {
        status = -1;
    }
    let bytes = if status == 1 && !owner.is_null() {
        // Owned DATA uses the same representation as a local owned message.
        Some(StreamPayload::View(views::Owned::new(owner)))
    } else if status == 1 {
        match udp_metadata(&event) {
            Ok(parsed) => Some(StreamPayload::Udp(Box::new(UdpOutput {
                event: parsed,
                payload: (!owned && event.kind == 1).then_some(NativeBuf(event.payload)),
                message: (!owned && event.kind == 1).then_some(NativeBuf(message)),
            }))),
            Err(error) => {
                unsafe {
                    plsm_udp_event_free(&mut event);
                    plsm_buf_free(&mut message);
                }
                return Err(error);
            }
        }
    } else {
        unsafe {
            plsm_udp_event_free(&mut event);
            plsm_buf_free(&mut message);
        }
        None
    };
    Ok(StreamReadOutput {
        status,
        bytes,
        seq: 0,
        timestamp_ns: 0,
        flags: 0,
        error: error as usize,
    })
}

fn udp_metadata(event: &plsm_udp_event_t) -> Result<UdpEvent> {
    let mut parsed = UdpEvent {
        kind: String::new(),
        seq: None,
        timestamp_ns: None,
        payload: None,
        message: None,
        first: None,
        last: None,
        reason: None,
        start_seq: None,
        next_seq: None,
        end_seq: None,
        terminal: None,
        retention_gap: None,
    };
    match event.kind {
        1 => {
            parsed.kind = "frame".to_string();
            parsed.seq = Some(BigInt::from(event.seq));
            parsed.timestamp_ns = Some(BigInt::from(event.timestamp_ns));
        }
        2 => {
            parsed.kind = "gap".to_string();
            parsed.first = Some(BigInt::from(event.first));
            parsed.last = Some(BigInt::from(event.last));
            parsed.reason = Some(
                match event.reason {
                    1 => "oversized",
                    2 => "retention",
                    3 => "pressure",
                    _ => "unknown",
                }
                .to_string(),
            );
        }
        3 => {
            parsed.kind = "progress".to_string();
            parsed.start_seq = Some(BigInt::from(event.start_seq));
            parsed.next_seq = Some(BigInt::from(event.next_seq));
            parsed.end_seq = (event.has_end != 0).then(|| BigInt::from(event.end_seq));
            parsed.terminal = Some(event.terminal != 0);
            parsed.retention_gap = Some(event.retention_gap != 0);
        }
        _ => return Err(Error::new(Status::GenericFailure, "unknown UDP event kind")),
    }
    Ok(parsed)
}

struct CStringArray {
    ptrs: Vec<*const c_char>,
    _strings: Vec<CString>,
}

impl CStringArray {
    fn new(values: &[String]) -> Result<Self> {
        let mut c_strings = Vec::with_capacity(values.len());
        for value in values {
            let cstr = CString::new(value.as_str())
                .map_err(|_| Error::new(Status::InvalidArg, "tags contains NUL"))?;
            c_strings.push(cstr);
        }
        let ptrs = c_strings.iter().map(|s| s.as_ptr()).collect();
        Ok(Self {
            ptrs,
            _strings: c_strings,
        })
    }

    fn as_ptr(&self) -> *const *const c_char {
        if self.ptrs.is_empty() {
            ptr::null()
        } else {
            self.ptrs.as_ptr()
        }
    }
}

// The native library owns this allocation. Moving the guard transfers its sole
// ownership; dropping it always calls that library's allocator-matched free.
struct NativeBuf(plsm_buf_t);

// SAFETY: This is an exclusively owned byte allocation, with no JS references.
// A worker may move it to the environment thread; only the final owner frees it.
unsafe impl Send for NativeBuf {}

impl Drop for NativeBuf {
    fn drop(&mut self) {
        unsafe { plsm_buf_free(&mut self.0) };
    }
}

unsafe extern "C" fn finalize_native_buf(
    _env: napi::sys::napi_env,
    _data: *mut libc::c_void,
    hint: *mut libc::c_void,
) {
    drop(unsafe { Box::from_raw(hint.cast::<NativeBuf>()) });
}

impl NativeBuf {
    fn copy_bytes(self) -> Vec<u8> {
        if self.0.len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(self.0.data, self.0.len) }.to_vec()
        }
    }

    fn into_node(self, env: Env) -> Result<JsBuffer> {
        if self.0.len == 0 {
            return Ok(env.create_buffer(0)?.into_raw());
        }
        let owner = Box::into_raw(Box::new(self));
        let mut value = ptr::null_mut();
        let status = unsafe {
            napi::sys::napi_create_external_buffer(
                env.raw(),
                (*owner).0.len,
                (*owner).0.data.cast(),
                Some(finalize_native_buf),
                owner.cast(),
                &mut value,
            )
        };
        if status == napi::sys::Status::napi_ok {
            // Return the existing JS handle without creating a temporary napi_ref.
            return Ok(unsafe { JsBuffer::from_raw_unchecked(env.raw(), value) });
        }
        let owner = unsafe { Box::from_raw(owner) };
        if status == napi::sys::Status::napi_no_external_buffers_allowed {
            let bytes = unsafe { std::slice::from_raw_parts(owner.0.data, owner.0.len) };
            return Ok(env.create_buffer_copy(bytes)?.into_raw());
        }
        Err(Error::new(
            Status::from(status),
            "failed to transfer native buffer",
        ))
    }
}

// Compact serialized JSON before JS retains it. Release measurements show no
// speed gain from transferring these buffers and higher retained memory for
// small JSON. Raw Lite3 frames already own a compact snapshot.
fn copy_json_buf(buf: plsm_buf_t) -> Buffer {
    Buffer::from(NativeBuf(buf).copy_bytes())
}

fn take_frame(env: Env, frame: plsm_lite3_frame_t) -> Result<Lite3Frame> {
    Ok(Lite3Frame {
        seq: BigInt::from(frame.seq),
        timestamp_ns: BigInt::from(frame.timestamp_ns),
        flags: frame.flags,
        payload: NativeBuf(frame.payload).into_node(env)?,
    })
}

fn take_error(env: Env, err: *mut plsm_error_t) -> Result<Error> {
    if err.is_null() {
        return Ok(Error::new(
            Status::GenericFailure,
            "plasmite: unknown error",
        ));
    }
    let owned = unsafe { &*err };
    let mut message = unsafe { cstring_to_string(owned.message) };
    let path = unsafe { cstring_to_string(owned.path) };
    let kind = error_kind_label(owned.kind);
    let seq = (owned.has_seq != 0).then_some(owned.seq);
    let offset = (owned.has_offset != 0).then_some(owned.offset);
    unsafe { plsm_error_free(err) };
    if message.is_empty() {
        message = default_error_message(kind).to_string();
    }
    // Human formatting does not define the machine error fields.
    let mut details = vec![format!("kind={kind}"), format!("message={message}")];
    if !path.is_empty() {
        details.push(format!("path={path}"));
    }
    if let Some(seq) = seq {
        details.push(format!("seq={seq}"));
    }
    if let Some(offset) = offset {
        details.push(format!("offset={offset}"));
    }
    let mut object = env.create_error(Error::new(
        Status::GenericFailure,
        format!("plasmite error: {}", details.join("; ")),
    ))?;
    object.set_named_property("kind", kind)?;
    if !path.is_empty() {
        object.set_named_property("path", path)?;
    }
    if let Some(seq) = seq {
        object.set_named_property("seq", BigInt::from(seq))?;
    }
    if let Some(offset) = offset {
        object.set_named_property("offset", BigInt::from(offset))?;
    }
    Ok(Error::from(object.into_unknown()))
}

fn default_error_message(kind: &str) -> &'static str {
    match kind {
        "Internal" => "internal error",
        "Usage" => "usage error",
        "NotFound" => "not found",
        "AlreadyExists" => "already exists",
        "Busy" => "busy",
        "Permission" => "permission denied",
        "Corrupt" => "corrupt",
        "Io" => "io error",
        "RetentionGap" => "retention gap",
        _ => "error",
    }
}

fn error_kind_label(kind: i32) -> &'static str {
    match kind {
        1 => "Internal",
        2 => "Usage",
        3 => "NotFound",
        4 => "AlreadyExists",
        5 => "Busy",
        6 => "Permission",
        7 => "Corrupt",
        8 => "Io",
        9 => "RetentionGap",
        _ => "Internal",
    }
}

fn stream_options(
    since_seq: Option<u64>,
    max_messages: Option<u64>,
    timeout_ms: Option<u64>,
    error_on_gap: bool,
    start_kind: u32,
    finite: bool,
) -> plsm_follow_options_t {
    plsm_follow_options_t {
        struct_size: std::mem::size_of::<plsm_follow_options_t>() as u32,
        gap_policy: u32::from(error_on_gap),
        since_seq: since_seq.unwrap_or(0),
        max_messages: max_messages.unwrap_or(0),
        timeout_ms: timeout_ms.unwrap_or(0),
        has_since: start_kind,
        has_max: u32::from(max_messages.is_some()),
        has_timeout: u32::from(timeout_ms.is_some()),
        reserved: u32::from(finite),
    }
}

unsafe fn cstring_to_string(ptr: *mut c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().to_string()
}

fn to_u64(value: Either<u32, BigInt>, name: &str) -> Result<u64> {
    match value {
        Either::A(number) => Ok(number as u64),
        Either::B(bigint) => {
            let (is_negative, value, lossless) = bigint.get_u64();
            if !is_negative && lossless {
                Ok(value)
            } else {
                Err(Error::new(
                    Status::InvalidArg,
                    format!("{name} must be non-negative"),
                ))
            }
        }
    }
}

pub struct GetViewTask {
    handle: Arc<PoolHandle>,
    seq: u64,
}
pub struct GetViewOutput {
    owner: Option<views::Owned>,
    error: usize,
}
impl Drop for GetViewOutput {
    fn drop(&mut self) {
        if self.error != 0 {
            unsafe { plsm_error_free(self.error as *mut plsm_error_t) }
        }
    }
}
impl Task for GetViewTask {
    type Output = GetViewOutput;
    type JsValue = views::NativeView;
    fn compute(&mut self) -> Result<Self::Output> {
        let _operation = self
            .handle
            .operation
            .lock()
            .map_err(|_| Error::from_reason("pool operation lock poisoned"))?;
        let mut owner = ptr::null();
        let mut error = ptr::null_mut();
        let rc =
            unsafe { views::plsm_pool_get_view(self.handle.ptr, self.seq, &mut owner, &mut error) };
        Ok(GetViewOutput {
            owner: if rc == 0 {
                Some(views::Owned::new(owner))
            } else {
                None
            },
            error: error as usize,
        })
    }
    fn resolve(&mut self, env: Env, output: Self::Output) -> Result<Self::JsValue> {
        resolve_owned_view(env, output)
    }
}

fn resolve_owned_view(env: Env, mut output: GetViewOutput) -> Result<views::NativeView> {
    if output.error != 0 {
        let err = std::mem::replace(&mut output.error, 0);
        return Err(take_error(env, err as *mut plsm_error_t)?);
    }
    views::NativeView::new(
        env,
        output
            .owner
            .take()
            .ok_or_else(|| Error::from_reason("native operation returned no owner"))?,
    )
}
