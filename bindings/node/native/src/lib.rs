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
use napi::{Env, Error, Result, Task};
use napi_derive::napi;
use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::Arc;

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
struct plsm_stream_options_t {
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

unsafe extern "C" {
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

    fn plsm_stream_open(
        pool: *mut plsm_pool_t,
        since_seq: u64,
        has_since: u32,
        max_messages: u64,
        has_max: u32,
        timeout_ms: u64,
        has_timeout: u32,
        out_stream: *mut *mut plsm_stream_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_stream_open_ex(
        pool: *mut plsm_pool_t,
        options: *const plsm_stream_options_t,
        out_stream: *mut *mut plsm_stream_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;

    fn plsm_lite3_stream_open(
        pool: *mut plsm_pool_t,
        since_seq: u64,
        has_since: u32,
        max_messages: u64,
        has_max: u32,
        timeout_ms: u64,
        has_timeout: u32,
        out_stream: *mut *mut plsm_lite3_stream_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;
    fn plsm_lite3_stream_open_ex(
        pool: *mut plsm_pool_t,
        options: *const plsm_stream_options_t,
        out_stream: *mut *mut plsm_lite3_stream_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;

    fn plsm_stream_next(
        stream: *mut plsm_stream_t,
        out_message: *mut plsm_buf_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;

    fn plsm_lite3_stream_next(
        stream: *mut plsm_lite3_stream_t,
        out_frame: *mut plsm_lite3_frame_t,
        out_err: *mut *mut plsm_error_t,
    ) -> c_int;

    fn plsm_stream_free(stream: *mut plsm_stream_t);
    fn plsm_lite3_stream_free(stream: *mut plsm_lite3_stream_t);

    fn plsm_buf_free(buf: *mut plsm_buf_t);
    fn plsm_lite3_frame_free(frame: *mut plsm_lite3_frame_t);
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
        let pool_dir = CString::new(pool_dir).map_err(|_| Error::new(Status::InvalidArg, "pool_dir contains NUL"))?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_client_new(pool_dir.as_ptr(), &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(Self { ptr: out })
    }

    #[napi]
    pub fn create_pool(&self, env: Env, pool_ref: String, size_bytes: Either<u32, BigInt>) -> Result<Pool> {
        let pool_ref = CString::new(pool_ref).map_err(|_| Error::new(Status::InvalidArg, "pool_ref contains NUL"))?;
        let size_bytes = to_u64(size_bytes, "size_bytes")?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_pool_create(self.ptr, pool_ref.as_ptr(), size_bytes, &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(Pool { ptr: out })
    }

    #[napi]
    pub fn open_pool(&self, env: Env, pool_ref: String) -> Result<Pool> {
        let pool_ref = CString::new(pool_ref).map_err(|_| Error::new(Status::InvalidArg, "pool_ref contains NUL"))?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_pool_open(self.ptr, pool_ref.as_ptr(), &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(Pool { ptr: out })
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
    ptr: *mut plsm_pool_t,
}

#[napi(object)]
pub struct Lite3Frame {
    pub seq: BigInt,
    #[napi(js_name = "timestampNs")]
    pub timestamp_ns: BigInt,
    pub flags: u32,
    pub payload: Buffer,
}

#[napi]
impl Pool {
    #[napi]
    pub fn append_json(&self, env: Env, payload: Buffer, tags: Vec<String>, durability: Durability) -> Result<Buffer> {
        let c_tags = CStringArray::new(&tags)?;
        let mut out = plsm_buf_t { data: ptr::null_mut(), len: 0 };
        let mut err = ptr::null_mut();
        let rc = unsafe {
            plsm_pool_append_json(
                self.ptr,
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
        Ok(copy_and_free_buf(out))
    }

    #[napi]
    pub fn append_lite3(&self, env: Env, payload: Buffer, durability: Durability) -> Result<BigInt> {
        let mut seq = 0u64;
        let mut err = ptr::null_mut();
        let rc = unsafe {
            plsm_pool_append_lite3(
                self.ptr,
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
    pub fn get_json(&self, env: Env, seq: Either<u32, BigInt>) -> Result<Buffer> {
        let seq = to_u64(seq, "seq")?;
        let mut out = plsm_buf_t { data: ptr::null_mut(), len: 0 };
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_pool_get_json(self.ptr, seq, &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(copy_and_free_buf(out))
    }

    #[napi]
    pub fn get_lite3(&self, env: Env, seq: Either<u32, BigInt>) -> Result<Lite3Frame> {
        let seq = to_u64(seq, "seq")?;
        let mut out = plsm_lite3_frame_t {
            seq: 0,
            timestamp_ns: 0,
            flags: 0,
            payload: plsm_buf_t { data: ptr::null_mut(), len: 0 },
        };
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_pool_get_lite3(self.ptr, seq, &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(copy_and_free_lite3_frame(out))
    }

    #[napi]
    pub fn open_stream(
        &self,
        env: Env,
        since_seq: Option<Either<u32, BigInt>>,
        max_messages: Option<Either<u32, BigInt>>,
        timeout_ms: Option<Either<u32, BigInt>>,
        error_on_gap: bool,
    ) -> Result<Stream> {
        let since_seq = since_seq.map(|value| to_u64(value, "since_seq")).transpose()?;
        let max_messages = max_messages.map(|value| to_u64(value, "max_messages")).transpose()?;
        let timeout_ms = timeout_ms.map(|value| to_u64(value, "timeout_ms")).transpose()?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = if error_on_gap {
            let options = stream_options(since_seq, max_messages, timeout_ms, true);
            unsafe { plsm_stream_open_ex(self.ptr, &options, &mut out, &mut err) }
        } else {
            unsafe {
                plsm_stream_open(
                    self.ptr,
                    since_seq.unwrap_or(0),
                    if since_seq.is_some() { 1 } else { 0 },
                    max_messages.unwrap_or(0),
                    if max_messages.is_some() { 1 } else { 0 },
                    timeout_ms.unwrap_or(0),
                    if timeout_ms.is_some() { 1 } else { 0 },
                    &mut out,
                    &mut err,
                )
            }
        };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(Stream { ptr: out })
    }

    #[napi]
    pub fn open_lite3_stream(
        &self,
        env: Env,
        since_seq: Option<Either<u32, BigInt>>,
        max_messages: Option<Either<u32, BigInt>>,
        timeout_ms: Option<Either<u32, BigInt>>,
        error_on_gap: bool,
    ) -> Result<Lite3Stream> {
        let since_seq = since_seq.map(|value| to_u64(value, "since_seq")).transpose()?;
        let max_messages = max_messages.map(|value| to_u64(value, "max_messages")).transpose()?;
        let timeout_ms = timeout_ms.map(|value| to_u64(value, "timeout_ms")).transpose()?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = if error_on_gap {
            let options = stream_options(since_seq, max_messages, timeout_ms, true);
            unsafe { plsm_lite3_stream_open_ex(self.ptr, &options, &mut out, &mut err) }
        } else {
            unsafe {
                plsm_lite3_stream_open(
                    self.ptr,
                    since_seq.unwrap_or(0),
                    if since_seq.is_some() { 1 } else { 0 },
                    max_messages.unwrap_or(0),
                    if max_messages.is_some() { 1 } else { 0 },
                    timeout_ms.unwrap_or(0),
                    if timeout_ms.is_some() { 1 } else { 0 },
                    &mut out,
                    &mut err,
                )
            }
        };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(Lite3Stream { ptr: out })
    }

    #[napi]
    pub fn close(&mut self) {
        if !self.ptr.is_null() {
            unsafe { plsm_pool_free(self.ptr) };
            self.ptr = ptr::null_mut();
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.close();
    }
}

#[napi]
pub struct Stream {
    ptr: *mut plsm_stream_t,
}

#[napi]
impl Stream {
    #[napi]
    pub fn next_json(&self, env: Env) -> Result<Option<Buffer>> {
        let mut out = plsm_buf_t { data: ptr::null_mut(), len: 0 };
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_stream_next(self.ptr, &mut out, &mut err) };
        match rc {
            1 => Ok(Some(copy_and_free_buf(out))),
            0 => Ok(None),
            _ => Err(take_error(env, err)?),
        }
    }

    #[napi]
    pub fn close(&mut self) {
        if !self.ptr.is_null() {
            unsafe { plsm_stream_free(self.ptr) };
            self.ptr = ptr::null_mut();
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.close();
    }
}

#[napi]
pub struct Lite3Stream {
    ptr: *mut plsm_lite3_stream_t,
}

#[napi]
impl Lite3Stream {
    #[napi]
    pub fn next(&self, env: Env) -> Result<Option<Lite3Frame>> {
        let mut out = plsm_lite3_frame_t {
            seq: 0,
            timestamp_ns: 0,
            flags: 0,
            payload: plsm_buf_t { data: ptr::null_mut(), len: 0 },
        };
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_lite3_stream_next(self.ptr, &mut out, &mut err) };
        match rc {
            1 => Ok(Some(copy_and_free_lite3_frame(out))),
            0 => Ok(None),
            _ => Err(take_error(env, err)?),
        }
    }

    #[napi]
    pub fn close(&mut self) {
        if !self.ptr.is_null() {
            unsafe { plsm_lite3_stream_free(self.ptr) };
            self.ptr = ptr::null_mut();
        }
    }
}

impl Drop for Lite3Stream {
    fn drop(&mut self) {
        self.close();
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
        let rc = unsafe { plsm_udp_client_open_pool(self.ptr, pool_name.as_ptr(), &mut out, &mut err) };
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
        let rc = unsafe {
            plsm_udp_pool_send_lite3(pool, payload.as_ptr(), payload.len(), &mut err)
        };
        if rc != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(())
    }

    #[napi(ts_return_type = "Promise<UdpFollow>")]
    pub fn follow(
        &self,
        start_kind: u32,
        start_value: Either<u32, BigInt>,
        live: bool,
    ) -> Result<AsyncTask<UdpFollowTask>> {
        let start_value = to_u64(start_value, "start_value")?;
        Ok(AsyncTask::new(UdpFollowTask {
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
            plsm_udp_pool_follow(
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
            ptr: follow as *mut plsm_udp_follow_t,
            finished: false,
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
    ptr: *mut plsm_udp_follow_t,
    finished: bool,
}

#[napi]
impl UdpFollow {
    #[napi(js_name = "nextEvent")]
    pub fn next_event(&mut self, env: Env, timeout_ms: Either<u32, BigInt>) -> Result<UdpWait> {
        if self.ptr.is_null() || self.finished {
            return Ok(UdpWait { status: 2, event: None });
        }
        let timeout_ms = to_u64(timeout_ms, "timeout_ms")?;
        let mut event = plsm_udp_event_t {
            kind: 0,
            reason: 0,
            seq: 0,
            timestamp_ns: 0,
            first: 0,
            last: 0,
            start_seq: 0,
            next_seq: 0,
            end_seq: 0,
            has_end: 0,
            terminal: 0,
            retention_gap: 0,
            reserved: 0,
            payload: plsm_buf_t { data: ptr::null_mut(), len: 0 },
        };
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_udp_follow_next(self.ptr, timeout_ms, &mut event, &mut err) };
        match rc {
            0 => Ok(UdpWait { status: 0, event: None }),
            1 => {
                let parsed = udp_event_from_native(env, &mut event);
                unsafe { plsm_udp_event_free(&mut event) };
                Ok(UdpWait {
                    status: 1,
                    event: Some(parsed?),
                })
            }
            2 => {
                self.finished = true;
                Ok(UdpWait { status: 2, event: None })
            }
            3 => {
                self.finished = true;
                Ok(UdpWait { status: 3, event: None })
            }
            _ => Err(take_error(env, err)?),
        }
    }

    #[napi(getter, js_name = "isFinished")]
    pub fn is_finished(&self) -> bool {
        self.finished || self.ptr.is_null()
    }

    #[napi]
    pub fn cancel(&self) {
        if !self.ptr.is_null() {
            unsafe { plsm_udp_follow_cancel(self.ptr) };
        }
    }

    #[napi]
    pub fn close(&mut self) {
        if !self.ptr.is_null() {
            unsafe { plsm_udp_follow_close(self.ptr) };
            self.ptr = ptr::null_mut();
            self.finished = true;
        }
    }
}

impl Drop for UdpFollow {
    fn drop(&mut self) {
        self.close();
    }
}

fn udp_event_from_native(env: Env, event: &mut plsm_udp_event_t) -> Result<UdpEvent> {
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
            parsed.payload = Some(Buffer::from(copy_buf_bytes(&event.payload)));
            let mut message = plsm_buf_t { data: ptr::null_mut(), len: 0 };
            let mut err = ptr::null_mut();
            let rc = unsafe { plsm_udp_event_message(event, &mut message, &mut err) };
            if rc != 0 {
                return Err(take_error(env, err)?);
            }
            parsed.message = Some(copy_and_free_buf(message));
        }
        2 => {
            parsed.kind = "gap".to_string();
            parsed.first = Some(BigInt::from(event.first));
            parsed.last = Some(BigInt::from(event.last));
            parsed.reason = Some(match event.reason {
                1 => "oversized",
                2 => "retention",
                3 => "pressure",
                _ => "unknown",
            }.to_string());
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

fn copy_buf_bytes(buf: &plsm_buf_t) -> Vec<u8> {
    if buf.data.is_null() || buf.len == 0 {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(buf.data, buf.len) }.to_vec()
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

fn copy_and_free_buf(mut buf: plsm_buf_t) -> Buffer {
    let data = if buf.data.is_null() || buf.len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(buf.data, buf.len) }.to_vec()
    };
    unsafe { plsm_buf_free(&mut buf) };
    Buffer::from(data)
}

fn copy_and_free_lite3_frame(mut frame: plsm_lite3_frame_t) -> Lite3Frame {
    let seq = frame.seq;
    let timestamp_ns = frame.timestamp_ns;
    let flags = frame.flags;
    let payload = if frame.payload.data.is_null() || frame.payload.len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(frame.payload.data, frame.payload.len) }.to_vec()
    };
    unsafe { plsm_lite3_frame_free(&mut frame) };
    Lite3Frame {
        seq: BigInt::from(seq),
        timestamp_ns: BigInt::from(timestamp_ns),
        flags,
        payload: Buffer::from(payload),
    }
}

fn take_error(env: Env, err: *mut plsm_error_t) -> Result<Error> {
    if err.is_null() {
        return Ok(Error::new(Status::GenericFailure, "plasmite: unknown error"));
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
    if !path.is_empty() { details.push(format!("path={path}")); }
    if let Some(seq) = seq { details.push(format!("seq={seq}")); }
    if let Some(offset) = offset { details.push(format!("offset={offset}")); }
    let mut object = env.create_error(Error::new(
        Status::GenericFailure, format!("plasmite error: {}", details.join("; ")),
    ))?;
    object.set_named_property("kind", kind)?;
    if !path.is_empty() { object.set_named_property("path", path)?; }
    if let Some(seq) = seq { object.set_named_property("seq", seq as f64)?; }
    if let Some(offset) = offset { object.set_named_property("offset", offset as f64)?; }
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
) -> plsm_stream_options_t {
    plsm_stream_options_t {
        struct_size: std::mem::size_of::<plsm_stream_options_t>() as u32,
        gap_policy: u32::from(error_on_gap),
        since_seq: since_seq.unwrap_or(0),
        max_messages: max_messages.unwrap_or(0),
        timeout_ms: timeout_ms.unwrap_or(0),
        has_since: u32::from(since_seq.is_some()),
        has_max: u32::from(max_messages.is_some()),
        has_timeout: u32::from(timeout_ms.is_some()),
        reserved: 0,
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
