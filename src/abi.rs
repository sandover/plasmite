//! Purpose: C ABI bridge for bindings (libplasmite).
//! Exports: C-callable client/pool/stream functions and buffer/error helpers.
//! Role: Stable ABI surface for non-Rust bindings in v0.
//! Invariants: JSON bytes in/out; Lite3 bytes for fast paths; explicit free functions.
//! Invariants: Error kinds map 1:1 with core error kinds.
//! Notes: Remote pool refs are not supported in v0.
#![allow(clippy::result_large_err)]

use crate::api::{LocalClient, PoolApiExt, PoolOptions, PoolRef};
use crate::core::error::{Error, ErrorKind};
use crate::core::pool::Pool;
use crate::json::parse;
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
pub struct plsm_lite3_stream {
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
pub struct plsm_lite3_frame {
    seq: u64,
    timestamp_ns: u64,
    flags: u32,
    payload: plsm_buf,
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
    tags: *const *const c_char,
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
    let tags = match parse_descrips(tags, descrips_len) {
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
pub extern "C" fn plsm_pool_append_lite3(
    pool: *mut plsm_pool,
    payload: *const u8,
    payload_len: usize,
    durability: u32,
    out_seq: *mut u64,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let pool = match borrow_pool(pool, out_err) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    if out_seq.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_seq is null"),
        );
    }
    if payload.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("lite3_bytes is null"),
        );
    }
    let payload = unsafe { std::slice::from_raw_parts(payload, payload_len) };
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
    let seq = match pool.pool.append_lite3_now(payload, durability) {
        Ok(seq) => seq,
        Err(err) => return fail(out_err, err),
    };
    unsafe {
        *out_seq = seq;
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
pub extern "C" fn plsm_pool_get_lite3(
    pool: *mut plsm_pool,
    seq: u64,
    out_frame: *mut plsm_lite3_frame,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let pool = match borrow_pool(pool, out_err) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let frame = match pool.pool.get_lite3(seq) {
        Ok(frame) => frame,
        Err(err) => return fail(out_err, err),
    };
    if let Err(err) = write_lite3_frame(out_frame, frame) {
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
pub extern "C" fn plsm_lite3_stream_open(
    pool: *mut plsm_pool,
    since_seq: u64,
    has_since: u32,
    max_messages: u64,
    has_max: u32,
    timeout_ms: u64,
    has_timeout: u32,
    out_stream: *mut *mut plsm_lite3_stream,
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
    let handle = Box::new(plsm_lite3_stream {
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
pub extern "C" fn plsm_lite3_stream_next(
    stream: *mut plsm_lite3_stream,
    out_frame: *mut plsm_lite3_frame,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let stream = match borrow_lite3_stream(stream, out_err) {
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
                stream.seen += 1;
                if let Err(err) = write_lite3_frame(out_frame, frame) {
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
pub extern "C" fn plsm_lite3_stream_free(stream: *mut plsm_lite3_stream) {
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
pub extern "C" fn plsm_lite3_frame_free(frame: *mut plsm_lite3_frame) {
    if frame.is_null() {
        return;
    }
    unsafe {
        let frame = &mut *frame;
        plsm_buf_free(&mut frame.payload);
        frame.seq = 0;
        frame.timestamp_ns = 0;
        frame.flags = 0;
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

fn borrow_lite3_stream<'a>(
    stream: *mut plsm_lite3_stream,
    out_err: *mut *mut plsm_error,
) -> Result<&'a mut plsm_lite3_stream, i32> {
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
    parse::from_str(text).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid json")
            .with_source(err)
    })
}

fn parse_descrips(tags: *const *const c_char, len: usize) -> Result<Vec<String>, Error> {
    if tags.is_null() {
        return Ok(Vec::new());
    }
    let slice = unsafe { std::slice::from_raw_parts(tags, len) };
    let mut out = Vec::with_capacity(len);
    for item in slice {
        if item.is_null() {
            return Err(Error::new(ErrorKind::Usage).with_message("tags contains null"));
        }
        let value = unsafe { CStr::from_ptr(*item) }
            .to_str()
            .map_err(|_| Error::new(ErrorKind::Usage).with_message("tags invalid UTF-8"))?
            .to_string();
        out.push(value);
    }
    Ok(out)
}

fn message_from_frame(frame: &crate::api::FrameRef<'_>) -> Result<crate::api::Message, Error> {
    let doc = crate::api::Lite3DocRef::new(frame.payload);
    let meta_type = doc
        .type_at_key(0, "meta")
        .map_err(|err| err.with_message("missing meta"))?;
    if meta_type != crate::api::lite3::sys::LITE3_TYPE_OBJECT {
        return Err(Error::new(ErrorKind::Corrupt).with_message("meta is not object"));
    }

    let meta_ofs = doc
        .key_offset("meta")
        .map_err(|err| err.with_message("missing meta"))?;
    let descrips_ofs = doc
        .key_offset_at(meta_ofs, "tags")
        .map_err(|err| err.with_message("missing meta.tags"))?;
    let descrips_json = doc.to_json_at(descrips_ofs, false)?;
    let descrips_value: Value = parse::from_str(&descrips_json).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid payload json")
            .with_source(err)
    })?;
    let tags = descrips_value
        .as_array()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("meta.tags must be array"))?
        .iter()
        .map(|item| item.as_str().map(|s| s.to_string()))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            Error::new(ErrorKind::Corrupt).with_message("meta.tags must be string array")
        })?;

    let data_ofs = doc
        .key_offset("data")
        .map_err(|err| err.with_message("missing data"))?;
    let data_json = doc.to_json_at(data_ofs, false)?;
    let data: Value = parse::from_str(&data_json).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid payload json")
            .with_source(err)
    })?;

    Ok(crate::api::Message {
        seq: frame.seq,
        time: format_ts(frame.timestamp_ns)?,
        meta: crate::api::Meta { tags },
        data,
    })
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
        "meta": { "tags": message.meta.tags },
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

fn write_lite3_frame(
    out_frame: *mut plsm_lite3_frame,
    frame: crate::api::FrameRef<'_>,
) -> Result<(), Error> {
    if out_frame.is_null() {
        return Err(Error::new(ErrorKind::Usage).with_message("out_frame is null"));
    }
    unsafe {
        let out_frame = &mut *out_frame;
        let mut data = frame.payload.to_vec().into_boxed_slice();
        out_frame.payload.len = data.len();
        out_frame.payload.data = data.as_mut_ptr();
        out_frame.seq = frame.seq;
        out_frame.timestamp_ns = frame.timestamp_ns;
        out_frame.flags = frame.flags;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, CString};

    fn parse_buf(buf: &plsm_buf) -> Value {
        let slice = unsafe { std::slice::from_raw_parts(buf.data, buf.len) };
        let text = std::str::from_utf8(slice).expect("utf8");
        parse::from_str(text).expect("valid json")
    }

    fn take_lite3_payload(frame: &plsm_lite3_frame) -> Vec<u8> {
        let slice = unsafe { std::slice::from_raw_parts(frame.payload.data, frame.payload.len) };
        slice.to_vec()
    }

    fn take_error(err: *mut plsm_error) -> (i32, String, Option<String>, Option<u64>, Option<u64>) {
        assert!(!err.is_null(), "expected error");
        let owned = unsafe { &*err };
        let message = unsafe { CStr::from_ptr(owned.message) }
            .to_str()
            .unwrap_or("")
            .to_string();
        let path = if owned.path.is_null() {
            None
        } else {
            Some(
                unsafe { CStr::from_ptr(owned.path) }
                    .to_str()
                    .unwrap_or("")
                    .to_string(),
            )
        };
        let seq = if owned.has_seq != 0 {
            Some(owned.seq)
        } else {
            None
        };
        let offset = if owned.has_offset != 0 {
            Some(owned.offset)
        } else {
            None
        };
        let kind = owned.kind;
        plsm_error_free(err);
        (kind, message, path, seq, offset)
    }

    #[test]
    fn abi_smoke_append_and_get() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pool_dir = temp.path().join("pools");
        std::fs::create_dir_all(&pool_dir).expect("mkdir");

        let pool_dir_c = CString::new(pool_dir.to_string_lossy().as_ref()).expect("cstr");
        let mut client: *mut plsm_client = std::ptr::null_mut();
        let mut err: *mut plsm_error = std::ptr::null_mut();
        let rc = plsm_client_new(pool_dir_c.as_ptr(), &mut client, &mut err);
        assert_eq!(rc, 0, "client_new failed");
        assert!(!client.is_null());

        let pool_name = CString::new("abi-pool").expect("cstr");
        let mut pool: *mut plsm_pool = std::ptr::null_mut();
        let rc = plsm_pool_create(client, pool_name.as_ptr(), 1024 * 1024, &mut pool, &mut err);
        assert_eq!(rc, 0, "pool_create failed");
        assert!(!pool.is_null());

        let payload = CString::new(r#"{"x":1}"#).expect("payload");
        let tag = CString::new("tag").expect("tag");
        let tags = [tag.as_ptr()];
        let mut out = plsm_buf {
            data: std::ptr::null_mut(),
            len: 0,
        };
        let rc = plsm_pool_append_json(
            pool,
            payload.as_ptr() as *const u8,
            payload.as_bytes().len(),
            tags.as_ptr(),
            tags.len(),
            0,
            &mut out,
            &mut err,
        );
        assert_eq!(rc, 0, "append failed");
        let message = parse_buf(&out);
        plsm_buf_free(&mut out);
        assert_eq!(message.get("data").unwrap()["x"], 1);

        let mut out = plsm_buf {
            data: std::ptr::null_mut(),
            len: 0,
        };
        let rc = plsm_pool_get_json(pool, 1, &mut out, &mut err);
        assert_eq!(rc, 0, "get failed");
        let message = parse_buf(&out);
        plsm_buf_free(&mut out);
        assert_eq!(message.get("seq").and_then(|v| v.as_u64()), Some(1));

        plsm_pool_free(pool);
        plsm_client_free(client);
        if !err.is_null() {
            plsm_error_free(err);
        }
    }

    #[test]
    fn abi_smoke_append_get_lite3() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pool_dir = temp.path().join("pools");
        std::fs::create_dir_all(&pool_dir).expect("mkdir");

        let pool_dir_c = CString::new(pool_dir.to_string_lossy().as_ref()).expect("cstr");
        let mut client: *mut plsm_client = std::ptr::null_mut();
        let mut err: *mut plsm_error = std::ptr::null_mut();
        let rc = plsm_client_new(pool_dir_c.as_ptr(), &mut client, &mut err);
        assert_eq!(rc, 0, "client_new failed");

        let pool_name = CString::new("abi-lite3").expect("cstr");
        let mut pool: *mut plsm_pool = std::ptr::null_mut();
        let rc = plsm_pool_create(client, pool_name.as_ptr(), 1024 * 1024, &mut pool, &mut err);
        assert_eq!(rc, 0, "pool_create failed");

        let payload =
            crate::core::lite3::encode_message(&["tag".to_string()], &serde_json::json!({"x": 1}))
                .expect("payload");
        let mut seq: u64 = 0;
        let rc = plsm_pool_append_lite3(
            pool,
            payload.as_slice().as_ptr(),
            payload.len(),
            0,
            &mut seq,
            &mut err,
        );
        assert_eq!(rc, 0, "append lite3 failed");
        assert_eq!(seq, 1);

        let mut frame = plsm_lite3_frame {
            seq: 0,
            timestamp_ns: 0,
            flags: 0,
            payload: plsm_buf {
                data: std::ptr::null_mut(),
                len: 0,
            },
        };
        let rc = plsm_pool_get_lite3(pool, 1, &mut frame, &mut err);
        assert_eq!(rc, 0, "get lite3 failed");
        assert_eq!(frame.seq, 1);
        assert_eq!(take_lite3_payload(&frame), payload.as_slice());
        plsm_lite3_frame_free(&mut frame);

        let mut stream: *mut plsm_lite3_stream = std::ptr::null_mut();
        let rc = plsm_lite3_stream_open(pool, 1, 1, 1, 1, 0, 0, &mut stream, &mut err);
        assert_eq!(rc, 0, "lite3 stream open failed");
        assert!(!stream.is_null());

        let mut frame = plsm_lite3_frame {
            seq: 0,
            timestamp_ns: 0,
            flags: 0,
            payload: plsm_buf {
                data: std::ptr::null_mut(),
                len: 0,
            },
        };
        let rc = plsm_lite3_stream_next(stream, &mut frame, &mut err);
        assert_eq!(rc, 1, "lite3 stream next failed");
        assert_eq!(frame.seq, 1);
        assert_eq!(take_lite3_payload(&frame), payload.as_slice());
        plsm_lite3_frame_free(&mut frame);
        plsm_lite3_stream_free(stream);

        plsm_pool_free(pool);
        plsm_client_free(client);
        if !err.is_null() {
            plsm_error_free(err);
        }
    }

    #[test]
    fn abi_errors_report_usage_on_null_pointers() {
        let mut err: *mut plsm_error = std::ptr::null_mut();
        let rc = plsm_client_new(std::ptr::null(), std::ptr::null_mut(), &mut err);
        assert_eq!(rc, -1);
        let (kind, message, _path, _seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::Usage));
        assert_eq!(message, "out_client is null");
    }

    #[test]
    fn abi_errors_report_invalid_pool_ref() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pool_dir = temp.path().join("pools");
        std::fs::create_dir_all(&pool_dir).expect("mkdir");
        let pool_dir_c = CString::new(pool_dir.to_string_lossy().as_ref()).expect("cstr");

        let mut client: *mut plsm_client = std::ptr::null_mut();
        let mut err: *mut plsm_error = std::ptr::null_mut();
        let rc = plsm_client_new(pool_dir_c.as_ptr(), &mut client, &mut err);
        assert_eq!(rc, 0, "client_new failed");

        let mut pool: *mut plsm_pool = std::ptr::null_mut();
        let rc = plsm_pool_open(client, std::ptr::null(), &mut pool, &mut err);
        assert_eq!(rc, -1);
        let (kind, message, _path, _seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::Usage));
        assert_eq!(message, "pool_ref is null");

        let remote = CString::new("http://example.com/pool").expect("cstr");
        let rc = plsm_pool_open(client, remote.as_ptr(), &mut pool, &mut err);
        assert_eq!(rc, -1);
        let (kind, message, _path, _seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::Usage));
        assert_eq!(message, "remote pool refs are not supported in v0");

        plsm_client_free(client);
    }

    #[test]
    fn abi_errors_include_path_for_missing_pool() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pool_dir = temp.path().join("pools");
        std::fs::create_dir_all(&pool_dir).expect("mkdir");
        let pool_dir_c = CString::new(pool_dir.to_string_lossy().as_ref()).expect("cstr");

        let mut client: *mut plsm_client = std::ptr::null_mut();
        let mut err: *mut plsm_error = std::ptr::null_mut();
        let rc = plsm_client_new(pool_dir_c.as_ptr(), &mut client, &mut err);
        assert_eq!(rc, 0, "client_new failed");

        let pool_name = CString::new("missing").expect("cstr");
        let mut pool: *mut plsm_pool = std::ptr::null_mut();
        let rc = plsm_pool_open(client, pool_name.as_ptr(), &mut pool, &mut err);
        assert_eq!(rc, -1);
        let (kind, _message, path, _seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::NotFound));
        let path = path.expect("path");
        assert!(path.ends_with("missing.plasmite"));

        plsm_client_free(client);
    }

    #[test]
    fn abi_errors_report_seq_on_missing_message() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pool_dir = temp.path().join("pools");
        std::fs::create_dir_all(&pool_dir).expect("mkdir");
        let pool_dir_c = CString::new(pool_dir.to_string_lossy().as_ref()).expect("cstr");

        let mut client: *mut plsm_client = std::ptr::null_mut();
        let mut err: *mut plsm_error = std::ptr::null_mut();
        let rc = plsm_client_new(pool_dir_c.as_ptr(), &mut client, &mut err);
        assert_eq!(rc, 0, "client_new failed");

        let pool_name = CString::new("seq").expect("cstr");
        let mut pool: *mut plsm_pool = std::ptr::null_mut();
        let rc = plsm_pool_create(client, pool_name.as_ptr(), 1024 * 1024, &mut pool, &mut err);
        assert_eq!(rc, 0, "pool_create failed");

        let mut out = plsm_buf {
            data: std::ptr::null_mut(),
            len: 0,
        };
        let rc = plsm_pool_get_json(pool, 42, &mut out, &mut err);
        assert_eq!(rc, -1);
        let (kind, _message, _path, seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::NotFound));
        assert_eq!(seq, Some(42));

        plsm_pool_free(pool);
        plsm_client_free(client);
    }

    #[test]
    fn abi_errors_report_invalid_json() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pool_dir = temp.path().join("pools");
        std::fs::create_dir_all(&pool_dir).expect("mkdir");
        let pool_dir_c = CString::new(pool_dir.to_string_lossy().as_ref()).expect("cstr");

        let mut client: *mut plsm_client = std::ptr::null_mut();
        let mut err: *mut plsm_error = std::ptr::null_mut();
        let rc = plsm_client_new(pool_dir_c.as_ptr(), &mut client, &mut err);
        assert_eq!(rc, 0, "client_new failed");

        let pool_name = CString::new("json").expect("cstr");
        let mut pool: *mut plsm_pool = std::ptr::null_mut();
        let rc = plsm_pool_create(client, pool_name.as_ptr(), 1024 * 1024, &mut pool, &mut err);
        assert_eq!(rc, 0, "pool_create failed");

        let mut out = plsm_buf {
            data: std::ptr::null_mut(),
            len: 0,
        };
        let rc = plsm_pool_append_json(
            pool,
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            0,
            &mut out,
            &mut err,
        );
        assert_eq!(rc, -1);
        let (kind, message, _path, _seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::Usage));
        assert_eq!(message, "json_bytes is null");

        plsm_pool_free(pool);
        plsm_client_free(client);
    }

    #[test]
    fn abi_errors_report_invalid_lite3() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pool_dir = temp.path().join("pools");
        std::fs::create_dir_all(&pool_dir).expect("mkdir");
        let pool_dir_c = CString::new(pool_dir.to_string_lossy().as_ref()).expect("cstr");

        let mut client: *mut plsm_client = std::ptr::null_mut();
        let mut err: *mut plsm_error = std::ptr::null_mut();
        let rc = plsm_client_new(pool_dir_c.as_ptr(), &mut client, &mut err);
        assert_eq!(rc, 0, "client_new failed");

        let pool_name = CString::new("lite3-invalid").expect("cstr");
        let mut pool: *mut plsm_pool = std::ptr::null_mut();
        let rc = plsm_pool_create(client, pool_name.as_ptr(), 1024 * 1024, &mut pool, &mut err);
        assert_eq!(rc, 0, "pool_create failed");

        let mut seq: u64 = 0;
        let rc = plsm_pool_append_lite3(pool, std::ptr::null(), 0, 0, &mut seq, &mut err);
        assert_eq!(rc, -1);
        let (kind, message, _path, _seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::Usage));
        assert_eq!(message, "lite3_bytes is null");

        let payload = [0u8; 8];
        let rc =
            plsm_pool_append_lite3(pool, payload.as_ptr(), payload.len(), 0, &mut seq, &mut err);
        assert_eq!(rc, -1);
        let (kind, _message, _path, _seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::Corrupt));

        plsm_pool_free(pool);
        plsm_client_free(client);
    }
}
