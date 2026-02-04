//! Purpose: C ABI bridge for bindings (libplasmite).
//! Exports: C-callable client/pool/stream functions and buffer/error helpers.
//! Role: Stable ABI surface for non-Rust bindings in v0.
//! Invariants: JSON bytes in/out; opaque handles; explicit free functions.
//! Invariants: Error kinds map 1:1 with core error kinds.
//! Notes: Remote pool refs are not supported in v0.
#![allow(clippy::result_large_err)]

use crate::api::{LocalClient, PoolApiExt, PoolOptions, PoolRef};
use crate::core::error::{Error, ErrorKind};
use crate::core::pool::Pool;
use serde_json::Value;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::ptr;
use std::time::{Duration, Instant};

#[repr(C)]
pub struct plsm_client {
    client: LocalClient,
}

#[repr(C)]
pub struct plsm_pool {
    pool: Pool,
}

#[repr(C)]
pub struct plsm_stream {
    pool: Pool,
    cursor: crate::api::Cursor,
    since_seq: Option<u64>,
    max_messages: Option<usize>,
    seen: usize,
    poll_interval: Duration,
    deadline: Option<Instant>,
}

#[repr(C)]
pub struct plsm_buf {
    data: *mut u8,
    len: usize,
}

#[repr(C)]
pub struct plsm_error {
    kind: i32,
    message: *mut c_char,
    path: *mut c_char,
    seq: u64,
    offset: u64,
    has_seq: u8,
    has_offset: u8,
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_client_new(
    pool_dir: *const c_char,
    out_client: *mut *mut plsm_client,
    out_err: *mut *mut plsm_error,
) -> i32 {
    if out_client.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_client is null"),
        );
    }
    let client = if pool_dir.is_null() {
        LocalClient::new()
    } else {
        let dir = unsafe { CStr::from_ptr(pool_dir) }
            .to_str()
            .map_err(|_| Error::new(ErrorKind::Usage).with_message("pool_dir is not valid UTF-8"));
        match dir {
            Ok(dir) => LocalClient::new().with_pool_dir(PathBuf::from(dir)),
            Err(err) => return fail(out_err, err),
        }
    };
    let handle = Box::new(plsm_client { client });
    unsafe {
        *out_client = Box::into_raw(handle);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_client_free(client: *mut plsm_client) {
    if client.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(client));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_create(
    client: *mut plsm_client,
    pool_ref: *const c_char,
    size_bytes: u64,
    out_pool: *mut *mut plsm_pool,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let client = match borrow_client(client, out_err) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let pool_ref = match parse_pool_ref(pool_ref, out_err) {
        Ok(pool_ref) => pool_ref,
        Err(code) => return code,
    };
    let size = if size_bytes == 0 {
        1024 * 1024
    } else {
        size_bytes
    };
    let options = PoolOptions::new(size);
    let pool = match client.client.create_pool(&pool_ref, options) {
        Ok(_) => match client.client.open_pool(&pool_ref) {
            Ok(pool) => pool,
            Err(err) => return fail(out_err, err),
        },
        Err(err) => return fail(out_err, err),
    };
    if out_pool.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_pool is null"),
        );
    }
    let handle = Box::new(plsm_pool { pool });
    unsafe {
        *out_pool = Box::into_raw(handle);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_open(
    client: *mut plsm_client,
    pool_ref: *const c_char,
    out_pool: *mut *mut plsm_pool,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let client = match borrow_client(client, out_err) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let pool_ref = match parse_pool_ref(pool_ref, out_err) {
        Ok(pool_ref) => pool_ref,
        Err(code) => return code,
    };
    let pool = match client.client.open_pool(&pool_ref) {
        Ok(pool) => pool,
        Err(err) => return fail(out_err, err),
    };
    if out_pool.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_pool is null"),
        );
    }
    let handle = Box::new(plsm_pool { pool });
    unsafe {
        *out_pool = Box::into_raw(handle);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_free(pool: *mut plsm_pool) {
    if pool.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(pool));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_append_json(
    pool: *mut plsm_pool,
    json_bytes: *const u8,
    json_len: usize,
    descrips: *const *const c_char,
    descrips_len: usize,
    durability: u32,
    out_message: *mut plsm_buf,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let pool = match borrow_pool(pool, out_err) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let data = match parse_json_bytes(json_bytes, json_len) {
        Ok(value) => value,
        Err(err) => return fail(out_err, err),
    };
    let tags = match parse_descrips(descrips, descrips_len) {
        Ok(tags) => tags,
        Err(err) => return fail(out_err, err),
    };
    let durability = match durability {
        0 => crate::api::Durability::Fast,
        1 => crate::api::Durability::Flush,
        _ => {
            return fail(
                out_err,
                Error::new(ErrorKind::Usage).with_message("invalid durability"),
            );
        }
    };
    let message = match pool.pool.append_json_now(&data, &tags, durability) {
        Ok(message) => message,
        Err(err) => return fail(out_err, err),
    };
    if let Err(err) = write_message_buf(out_message, message) {
        return fail(out_err, err);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_get_json(
    pool: *mut plsm_pool,
    seq: u64,
    out_message: *mut plsm_buf,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let pool = match borrow_pool(pool, out_err) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let message = match pool.pool.get_message(seq) {
        Ok(message) => message,
        Err(err) => return fail(out_err, err),
    };
    if let Err(err) = write_message_buf(out_message, message) {
        return fail(out_err, err);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_stream_open(
    pool: *mut plsm_pool,
    since_seq: u64,
    has_since: u32,
    max_messages: u64,
    has_max: u32,
    timeout_ms: u64,
    has_timeout: u32,
    out_stream: *mut *mut plsm_stream,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let pool = match borrow_pool(pool, out_err) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    if out_stream.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_stream is null"),
        );
    }
    let since = if has_since != 0 {
        Some(since_seq)
    } else {
        None
    };
    let max = if has_max != 0 {
        Some(max_messages as usize)
    } else {
        None
    };
    let timeout = if has_timeout != 0 {
        Some(Duration::from_millis(timeout_ms))
    } else {
        None
    };
    let deadline = timeout.map(|duration| Instant::now() + duration);
    let info = match pool.pool.info() {
        Ok(info) => info,
        Err(err) => return fail(out_err, err),
    };
    let pool = match Pool::open(&info.path) {
        Ok(pool) => pool,
        Err(err) => return fail(out_err, err),
    };
    let handle = Box::new(plsm_stream {
        pool,
        cursor: crate::api::Cursor::new(),
        since_seq: since,
        max_messages: max,
        seen: 0,
        poll_interval: Duration::from_millis(50),
        deadline,
    });
    unsafe {
        *out_stream = Box::into_raw(handle);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_stream_next(
    stream: *mut plsm_stream,
    out_message: *mut plsm_buf,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let stream = match borrow_stream(stream, out_err) {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    if let Some(max) = stream.max_messages {
        if stream.seen >= max {
            return 0;
        }
    }

    loop {
        if let Some(deadline) = stream.deadline {
            if Instant::now() >= deadline {
                return 0;
            }
        }

        match stream.cursor.next(&stream.pool) {
            Ok(crate::api::CursorResult::Message(frame)) => {
                if let Some(min_seq) = stream.since_seq {
                    if frame.seq < min_seq {
                        continue;
                    }
                }
                let message = match message_from_frame(&frame) {
                    Ok(message) => message,
                    Err(err) => return fail(out_err, err),
                };
                stream.seen += 1;
                if let Err(err) = write_message_buf(out_message, message) {
                    return fail(out_err, err);
                }
                return 1;
            }
            Ok(crate::api::CursorResult::WouldBlock) => {
                std::thread::sleep(stream.poll_interval);
            }
            Ok(crate::api::CursorResult::FellBehind) => continue,
            Err(err) => return fail(out_err, err),
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_stream_free(stream: *mut plsm_stream) {
    if stream.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(stream));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_buf_free(buf: *mut plsm_buf) {
    if buf.is_null() {
        return;
    }
    unsafe {
        let buf = &mut *buf;
        if !buf.data.is_null() && buf.len != 0 {
            drop(Vec::from_raw_parts(buf.data, buf.len, buf.len));
        }
        buf.data = ptr::null_mut();
        buf.len = 0;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_error_free(err: *mut plsm_error) {
    if err.is_null() {
        return;
    }
    unsafe {
        let err = Box::from_raw(err);
        if !err.message.is_null() {
            drop(CString::from_raw(err.message));
        }
        if !err.path.is_null() {
            drop(CString::from_raw(err.path));
        }
    }
}

fn borrow_client<'a>(
    client: *mut plsm_client,
    out_err: *mut *mut plsm_error,
) -> Result<&'a mut plsm_client, i32> {
    if client.is_null() {
        return Err(fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("client is null"),
        ));
    }
    unsafe { Ok(&mut *client) }
}

fn borrow_pool<'a>(
    pool: *mut plsm_pool,
    out_err: *mut *mut plsm_error,
) -> Result<&'a mut plsm_pool, i32> {
    if pool.is_null() {
        return Err(fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("pool is null"),
        ));
    }
    unsafe { Ok(&mut *pool) }
}

fn borrow_stream<'a>(
    stream: *mut plsm_stream,
    out_err: *mut *mut plsm_error,
) -> Result<&'a mut plsm_stream, i32> {
    if stream.is_null() {
        return Err(fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("stream is null"),
        ));
    }
    unsafe { Ok(&mut *stream) }
}

fn parse_pool_ref(input: *const c_char, out_err: *mut *mut plsm_error) -> Result<PoolRef, i32> {
    if input.is_null() {
        return Err(fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("pool_ref is null"),
        ));
    }
    let raw = unsafe { CStr::from_ptr(input) }
        .to_str()
        .map_err(|_| Error::new(ErrorKind::Usage).with_message("pool_ref is not valid UTF-8"))
        .map_err(|err| fail(out_err, err))?;
    if raw.contains("://") {
        return Err(fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("remote pool refs are not supported in v0"),
        ));
    }
    if raw.contains('/') {
        Ok(PoolRef::path(raw))
    } else {
        Ok(PoolRef::name(raw))
    }
}

fn parse_json_bytes(bytes: *const u8, len: usize) -> Result<Value, Error> {
    if bytes.is_null() {
        return Err(Error::new(ErrorKind::Usage).with_message("json_bytes is null"));
    }
    let slice = unsafe { std::slice::from_raw_parts(bytes, len) };
    let text = std::str::from_utf8(slice).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid json utf-8")
            .with_source(err)
    })?;
    serde_json::from_str(text).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid json")
            .with_source(err)
    })
}

fn parse_descrips(descrips: *const *const c_char, len: usize) -> Result<Vec<String>, Error> {
    if descrips.is_null() {
        return Ok(Vec::new());
    }
    let slice = unsafe { std::slice::from_raw_parts(descrips, len) };
    let mut out = Vec::with_capacity(len);
    for item in slice {
        if item.is_null() {
            return Err(Error::new(ErrorKind::Usage).with_message("descrips contains null"));
        }
        let value = unsafe { CStr::from_ptr(*item) }
            .to_str()
            .map_err(|_| Error::new(ErrorKind::Usage).with_message("descrips invalid UTF-8"))?
            .to_string();
        out.push(value);
    }
    Ok(out)
}

fn message_from_frame(frame: &crate::api::FrameRef<'_>) -> Result<crate::api::Message, Error> {
    let json_str = crate::api::Lite3DocRef::new(frame.payload).to_json(false)?;
    let value: Value = serde_json::from_str(&json_str).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid payload json")
            .with_source(err)
    })?;
    let obj = value
        .as_object()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("payload is not object"))?;
    let meta_value = obj
        .get("meta")
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("missing meta"))?;
    let data = obj
        .get("data")
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("missing data"))?;

    let meta_obj = meta_value
        .as_object()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("meta is not object"))?;
    let descrips_value = meta_obj
        .get("descrips")
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("missing meta.descrips"))?;
    let descrips = descrips_value
        .as_array()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("meta.descrips must be array"))?
        .iter()
        .map(|item| item.as_str().map(|s| s.to_string()))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            Error::new(ErrorKind::Corrupt).with_message("meta.descrips must be string array")
        })?;

    let message = crate::api::Message {
        seq: frame.seq,
        time: format_ts(frame.timestamp_ns)?,
        meta: crate::api::Meta { descrips },
        data,
    };
    Ok(message)
}

fn write_message_buf(
    out_message: *mut plsm_buf,
    message: crate::api::Message,
) -> Result<(), Error> {
    if out_message.is_null() {
        return Err(Error::new(ErrorKind::Usage).with_message("out_message is null"));
    }
    let json = serde_json::json!({
        "seq": message.seq,
        "time": message.time,
        "meta": { "descrips": message.meta.descrips },
        "data": message.data,
    });
    let json_bytes = serde_json::to_vec(&json).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("failed to serialize message")
            .with_source(err)
    })?;
    unsafe {
        let buf = &mut *out_message;
        let mut data = json_bytes.into_boxed_slice();
        buf.len = data.len();
        buf.data = data.as_mut_ptr();
        std::mem::forget(data);
    }
    Ok(())
}

fn fail(out_err: *mut *mut plsm_error, err: Error) -> i32 {
    if out_err.is_null() {
        return -1;
    }
    let error = Box::new(plsm_error {
        kind: error_kind_code(err.kind()),
        message: to_c_string(err.message().unwrap_or("")),
        path: err
            .path()
            .map(|path| to_c_string(path.to_string_lossy().as_ref()))
            .unwrap_or(ptr::null_mut()),
        seq: err.seq().unwrap_or(0),
        offset: err.offset().unwrap_or(0),
        has_seq: if err.seq().is_some() { 1 } else { 0 },
        has_offset: if err.offset().is_some() { 1 } else { 0 },
    });
    unsafe {
        *out_err = Box::into_raw(error);
    }
    -1
}

fn to_c_string(input: &str) -> *mut c_char {
    CString::new(input)
        .map(|s| s.into_raw())
        .unwrap_or(ptr::null_mut())
}

fn error_kind_code(kind: ErrorKind) -> i32 {
    match kind {
        ErrorKind::Internal => 1,
        ErrorKind::Usage => 2,
        ErrorKind::NotFound => 3,
        ErrorKind::AlreadyExists => 4,
        ErrorKind::Busy => 5,
        ErrorKind::Permission => 6,
        ErrorKind::Corrupt => 7,
        ErrorKind::Io => 8,
    }
}

fn format_ts(timestamp_ns: u64) -> Result<String, Error> {
    use time::format_description::well_known::Rfc3339;
    let ts =
        time::OffsetDateTime::from_unix_timestamp_nanos(timestamp_ns as i128).map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("invalid timestamp")
                .with_source(err)
        })?;
    ts.format(&Rfc3339).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("timestamp format failed")
            .with_source(err)
    })
}
