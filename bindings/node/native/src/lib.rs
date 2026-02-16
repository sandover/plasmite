/*
Purpose: Provide a Node N-API binding over the libplasmite C ABI.
Key Exports: Client, Pool, Stream, Durability, ErrorKind.
Role: Official Node/TypeScript binding that mirrors the v0 API contract.
Invariants: Calls into C ABI only; JSON bytes in/out; explicit Close methods.
Invariants: Errors include stable kinds and context in message text.
Notes: The addon links against libplasmite and does not re-implement internals.
*/

use libc::{c_char, c_int};
use napi::bindgen_prelude::{BigInt, Buffer, Either, Status};
use napi::{Error, Result};
use napi_derive::napi;
use std::ffi::{CStr, CString};
use std::ptr;

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
struct plsm_buf_t {
    data: *mut u8,
    len: usize,
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
}

#[napi]
pub struct Client {
    ptr: *mut plsm_client_t,
}

#[napi]
impl Client {
    #[napi(constructor)]
    pub fn new(pool_dir: String) -> Result<Self> {
        let pool_dir = CString::new(pool_dir).map_err(|_| Error::new(Status::InvalidArg, "pool_dir contains NUL"))?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_client_new(pool_dir.as_ptr(), &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(err));
        }
        Ok(Self { ptr: out })
    }

    #[napi]
    pub fn create_pool(&self, pool_ref: String, size_bytes: Either<u32, BigInt>) -> Result<Pool> {
        let pool_ref = CString::new(pool_ref).map_err(|_| Error::new(Status::InvalidArg, "pool_ref contains NUL"))?;
        let size_bytes = to_u64(size_bytes, "size_bytes")?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_pool_create(self.ptr, pool_ref.as_ptr(), size_bytes, &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(err));
        }
        Ok(Pool { ptr: out })
    }

    #[napi]
    pub fn open_pool(&self, pool_ref: String) -> Result<Pool> {
        let pool_ref = CString::new(pool_ref).map_err(|_| Error::new(Status::InvalidArg, "pool_ref contains NUL"))?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_pool_open(self.ptr, pool_ref.as_ptr(), &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(err));
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
    pub fn append_json(&self, payload: Buffer, tags: Vec<String>, durability: Durability) -> Result<Buffer> {
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
            return Err(take_error(err));
        }
        Ok(copy_and_free_buf(out))
    }

    #[napi]
    pub fn append_lite3(&self, payload: Buffer, durability: Durability) -> Result<BigInt> {
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
            return Err(take_error(err));
        }
        Ok(BigInt::from(seq))
    }

    #[napi]
    pub fn get_json(&self, seq: Either<u32, BigInt>) -> Result<Buffer> {
        let seq = to_u64(seq, "seq")?;
        let mut out = plsm_buf_t { data: ptr::null_mut(), len: 0 };
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_pool_get_json(self.ptr, seq, &mut out, &mut err) };
        if rc != 0 {
            return Err(take_error(err));
        }
        Ok(copy_and_free_buf(out))
    }

    #[napi]
    pub fn get_lite3(&self, seq: Either<u32, BigInt>) -> Result<Lite3Frame> {
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
            return Err(take_error(err));
        }
        Ok(copy_and_free_lite3_frame(out))
    }

    #[napi]
    pub fn open_stream(
        &self,
        since_seq: Option<Either<u32, BigInt>>,
        max_messages: Option<Either<u32, BigInt>>,
        timeout_ms: Option<Either<u32, BigInt>>,
    ) -> Result<Stream> {
        let since_seq = since_seq.map(|value| to_u64(value, "since_seq")).transpose()?;
        let max_messages = max_messages.map(|value| to_u64(value, "max_messages")).transpose()?;
        let timeout_ms = timeout_ms.map(|value| to_u64(value, "timeout_ms")).transpose()?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = unsafe {
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
        };
        if rc != 0 {
            return Err(take_error(err));
        }
        Ok(Stream { ptr: out })
    }

    #[napi]
    pub fn open_lite3_stream(
        &self,
        since_seq: Option<Either<u32, BigInt>>,
        max_messages: Option<Either<u32, BigInt>>,
        timeout_ms: Option<Either<u32, BigInt>>,
    ) -> Result<Lite3Stream> {
        let since_seq = since_seq.map(|value| to_u64(value, "since_seq")).transpose()?;
        let max_messages = max_messages.map(|value| to_u64(value, "max_messages")).transpose()?;
        let timeout_ms = timeout_ms.map(|value| to_u64(value, "timeout_ms")).transpose()?;
        let mut out = ptr::null_mut();
        let mut err = ptr::null_mut();
        let rc = unsafe {
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
        };
        if rc != 0 {
            return Err(take_error(err));
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
    pub fn next_json(&self) -> Result<Option<Buffer>> {
        let mut out = plsm_buf_t { data: ptr::null_mut(), len: 0 };
        let mut err = ptr::null_mut();
        let rc = unsafe { plsm_stream_next(self.ptr, &mut out, &mut err) };
        match rc {
            1 => Ok(Some(copy_and_free_buf(out))),
            0 => Ok(None),
            _ => Err(take_error(err)),
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
    pub fn next(&self) -> Result<Option<Lite3Frame>> {
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
            _ => Err(take_error(err)),
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

fn take_error(err: *mut plsm_error_t) -> Error {
    if err.is_null() {
        return Error::new(Status::GenericFailure, "plasmite: unknown error");
    }
    let owned = unsafe { &*err };
    let mut message = unsafe { cstring_to_string(owned.message) };
    let path = unsafe { cstring_to_string(owned.path) };
    let mut details = Vec::new();
    let kind_label = error_kind_label(owned.kind);
    details.push(format!("kind={}", kind_label));
    if message.is_empty() {
        message = default_error_message(kind_label).to_string();
    }
    details.push(format!("message={}", message));
    if !path.is_empty() {
        details.push(format!("path={}", path));
    }
    if owned.has_seq != 0 {
        details.push(format!("seq={}", owned.seq));
    }
    if owned.has_offset != 0 {
        details.push(format!("offset={}", owned.offset));
    }
    unsafe { plsm_error_free(err) };
    Error::new(Status::GenericFailure, format!("plasmite error: {}", details.join("; ")))
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
        _ => "Internal",
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
