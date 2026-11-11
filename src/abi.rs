//! Purpose: C ABI bridge for bindings (libplasmite).
//! Exports: C-callable client/pool/stream functions and buffer/error helpers.
//! Role: Stable ABI surface for non-Rust bindings in v0.
//!
//! ABI stability: additive-only within a major version — no field removals,
//! no struct reordering, no enum discriminant changes.  See `include/plasmite.h`
//! for the full stability contract, ownership rules, and linking notes.
//!
//! Invariants: JSON bytes in/out; Lite3 bytes for fast paths; explicit free functions.
//! Invariants: Error kinds map 1:1 with core error kinds.
//! Notes: Remote pool refs are not supported in v0.
#![allow(clippy::result_large_err)]

use crate::api::{
    GapPolicy, LocalClient, PoolApiExt, PoolOptions, PoolRef, TailOptions, TailOutcome, TailState,
};
use crate::core::error::{Error, ErrorKind};
use crate::core::pool::Pool;
use serde_json::Value;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
    state: Mutex<TailState>,
    cancel: Arc<AtomicBool>,
}

#[repr(C)]
pub struct plsm_lite3_stream {
    pool: Pool,
    state: Mutex<TailState>,
    cancel: Arc<AtomicBool>,
}

#[repr(C)]
pub struct plsm_udp_client {
    client: crate::api::UdpClient,
}

#[repr(C)]
pub struct plsm_udp_pool {
    pool: crate::api::UdpPool,
}

#[repr(C)]
pub struct plsm_udp_follow {
    follow: Mutex<crate::api::UdpFollow>,
    cancelled: AtomicBool,
}

#[repr(C)]
#[derive(Default)]
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
#[derive(Default)]
pub struct plsm_udp_event {
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

#[repr(C)]
pub struct plsm_stream_options {
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

#[derive(Clone, Copy)]
struct StreamOpenOptions {
    since_seq: Option<u64>,
    max_messages: Option<usize>,
    timeout: Option<Duration>,
    gap_policy: GapPolicy,
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
    if out_pool.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_pool is null"),
        );
    }
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
    tags_len: usize,
    durability: u32,
    out_message: *mut plsm_buf,
    out_err: *mut *mut plsm_error,
) -> i32 {
    if out_message.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_message is null"),
        );
    }
    let pool = match borrow_pool(pool, out_err) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let data = match parse_json_bytes(json_bytes, json_len) {
        Ok(value) => value,
        Err(err) => return fail(out_err, err),
    };
    let tags = match parse_tags(tags, tags_len) {
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
    let options = plsm_stream_options {
        struct_size: std::mem::size_of::<plsm_stream_options>() as u32,
        gap_policy: 0,
        since_seq,
        max_messages,
        timeout_ms,
        has_since,
        has_max,
        has_timeout,
        reserved: 0,
    };
    plsm_stream_open_ex(pool, &options, out_stream, out_err)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_stream_open_ex(
    pool: *mut plsm_pool,
    options: *const plsm_stream_options,
    out_stream: *mut *mut plsm_stream,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let options = match parse_stream_options(options, out_err) {
        Ok(options) => options,
        Err(code) => return code,
    };
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
    let cancel = Arc::new(AtomicBool::new(false));
    let state = TailState::new(
        &pool.pool,
        TailOptions {
            since_seq: options.since_seq,
            max_messages: options.max_messages,
            timeout: options.timeout,
            gap_policy: options.gap_policy,
            notify: false,
            cancel: Some(cancel.clone()),
            ..TailOptions::default()
        },
    );
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
        state: Mutex::new(state),
        cancel,
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
    if out_message.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_message is null"),
        );
    }
    let stream = match borrow_stream(stream, out_err) {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    match read_stream(stream, None) {
        Ok(TailOutcome::Item(message)) => match write_message_buf(out_message, message) {
            Ok(()) => 1,
            Err(err) => fail(out_err, err),
        },
        Ok(_) => 0,
        Err(err) => fail(out_err, err),
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
    let options = plsm_stream_options {
        struct_size: std::mem::size_of::<plsm_stream_options>() as u32,
        gap_policy: 0,
        since_seq,
        max_messages,
        timeout_ms,
        has_since,
        has_max,
        has_timeout,
        reserved: 0,
    };
    plsm_lite3_stream_open_ex(pool, &options, out_stream, out_err)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_lite3_stream_open_ex(
    pool: *mut plsm_pool,
    options: *const plsm_stream_options,
    out_stream: *mut *mut plsm_lite3_stream,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let options = match parse_stream_options(options, out_err) {
        Ok(options) => options,
        Err(code) => return code,
    };
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
    let cancel = Arc::new(AtomicBool::new(false));
    let state = TailState::new(
        &pool.pool,
        TailOptions {
            since_seq: options.since_seq,
            max_messages: options.max_messages,
            timeout: options.timeout,
            gap_policy: options.gap_policy,
            notify: false,
            cancel: Some(cancel.clone()),
            ..TailOptions::default()
        },
    );
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
        state: Mutex::new(state),
        cancel,
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
    if out_frame.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_frame is null"),
        );
    }
    let stream = match borrow_lite3_stream(stream, out_err) {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    match read_lite3_stream(stream, None) {
        Ok(TailOutcome::Item(frame)) => match write_lite3_frame(out_frame, frame) {
            Ok(()) => 1,
            Err(err) => fail(out_err, err),
        },
        Ok(_) => 0,
        Err(err) => fail(out_err, err),
    }
}

// Legacy and bounded waits use the same reader and retain the original open-time deadline.
fn read_stream(
    stream: &plsm_stream,
    wait: Option<Duration>,
) -> Result<TailOutcome<crate::api::Message>, Error> {
    let mut state = stream
        .state
        .lock()
        .map_err(|_| Error::new(ErrorKind::Internal).with_message("stream state is unavailable"))?;
    state.next_wait(&stream.pool, wait, |frame, _| {
        crate::api::message_from_frame(&frame).map(Some)
    })
}

fn read_lite3_stream(
    stream: &plsm_lite3_stream,
    wait: Option<Duration>,
) -> Result<TailOutcome<crate::api::FrameRef>, Error> {
    let mut state = stream
        .state
        .lock()
        .map_err(|_| Error::new(ErrorKind::Internal).with_message("stream state is unavailable"))?;
    state.next_wait(&stream.pool, wait, |frame, _| Ok(Some(frame)))
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_stream_next_wait(
    stream: *mut plsm_stream,
    timeout_ms: u64,
    out_message: *mut plsm_buf,
    out_err: *mut *mut plsm_error,
) -> i32 {
    if out_message.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_message is null"),
        );
    }
    let stream = match borrow_stream(stream, out_err) {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    match read_stream(stream, Some(Duration::from_millis(timeout_ms))) {
        Ok(TailOutcome::Item(message)) => match write_message_buf(out_message, message) {
            Ok(()) => 1,
            Err(err) => fail(out_err, err),
        },
        Ok(TailOutcome::WaitExpired) => 0,
        Ok(TailOutcome::Finished) => 2,
        Ok(TailOutcome::Cancelled) => 3,
        Err(err) => fail(out_err, err),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_lite3_stream_next_wait(
    stream: *mut plsm_lite3_stream,
    timeout_ms: u64,
    out_frame: *mut plsm_lite3_frame,
    out_err: *mut *mut plsm_error,
) -> i32 {
    if out_frame.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_frame is null"),
        );
    }
    let stream = match borrow_lite3_stream(stream, out_err) {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    match read_lite3_stream(stream, Some(Duration::from_millis(timeout_ms))) {
        Ok(TailOutcome::Item(frame)) => match write_lite3_frame(out_frame, frame) {
            Ok(()) => 1,
            Err(err) => fail(out_err, err),
        },
        Ok(TailOutcome::WaitExpired) => 0,
        Ok(TailOutcome::Finished) => 2,
        Ok(TailOutcome::Cancelled) => 3,
        Err(err) => fail(out_err, err),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_stream_cancel(stream: *mut plsm_stream) {
    if let Some(stream) = unsafe { stream.as_ref() } {
        stream.cancel.store(true, Ordering::Release);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_lite3_stream_cancel(stream: *mut plsm_lite3_stream) {
    if let Some(stream) = unsafe { stream.as_ref() } {
        stream.cancel.store(true, Ordering::Release);
    }
}

/// Open a UDP client for the explicit server endpoint, such as
/// `udp://127.0.0.1:9701`.
#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_client_new(
    endpoint: *const c_char,
    out_client: *mut *mut plsm_udp_client,
    out_err: *mut *mut plsm_error,
) -> i32 {
    if out_client.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_client is null"),
        );
    }
    let endpoint = match parse_utf8_cstr(endpoint, "endpoint") {
        Ok(endpoint) => endpoint,
        Err(err) => return fail(out_err, err),
    };
    let client = match crate::api::UdpClient::new(endpoint) {
        Ok(client) => client,
        Err(err) => return fail(out_err, err),
    };
    unsafe {
        *out_client = Box::into_raw(Box::new(plsm_udp_client { client }));
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_client_free(client: *mut plsm_udp_client) {
    if !client.is_null() {
        unsafe { drop(Box::from_raw(client)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_client_open_pool(
    client: *mut plsm_udp_client,
    pool_name: *const c_char,
    out_pool: *mut *mut plsm_udp_pool,
    out_err: *mut *mut plsm_error,
) -> i32 {
    if out_pool.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_pool is null"),
        );
    }
    if client.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("client is null"),
        );
    }
    let pool_name = match parse_utf8_cstr(pool_name, "pool_name") {
        Ok(pool_name) => pool_name,
        Err(err) => return fail(out_err, err),
    };
    let pool = match unsafe { &*client }
        .client
        .open_pool(&PoolRef::name(pool_name))
    {
        Ok(pool) => pool,
        Err(err) => return fail(out_err, err),
    };
    unsafe {
        *out_pool = Box::into_raw(Box::new(plsm_udp_pool { pool }));
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_pool_free(pool: *mut plsm_udp_pool) {
    if !pool.is_null() {
        unsafe { drop(Box::from_raw(pool)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_pool_send_json(
    pool: *mut plsm_udp_pool,
    json_bytes: *const u8,
    json_len: usize,
    tags: *const *const c_char,
    tags_len: usize,
    out_err: *mut *mut plsm_error,
) -> i32 {
    if pool.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("pool is null"),
        );
    }
    let data = match parse_json_bytes(json_bytes, json_len) {
        Ok(value) => value,
        Err(err) => return fail(out_err, err),
    };
    let tags = match parse_tags(tags, tags_len) {
        Ok(tags) => tags,
        Err(err) => return fail(out_err, err),
    };
    match unsafe { &*pool }.pool.send_json(&data, &tags) {
        Ok(()) => 0,
        Err(err) => fail(out_err, err),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_pool_send_lite3(
    pool: *mut plsm_udp_pool,
    payload: *const u8,
    payload_len: usize,
    out_err: *mut *mut plsm_error,
) -> i32 {
    if pool.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("pool is null"),
        );
    }
    if payload.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("lite3_bytes is null"),
        );
    }
    let payload = unsafe { std::slice::from_raw_parts(payload, payload_len) };
    match unsafe { &*pool }.pool.send_lite3(payload) {
        Ok(()) => 0,
        Err(err) => fail(out_err, err),
    }
}

/// Start-kind values are 0=New, 1=Last, 2=Since (nanoseconds), 3=Sequence.
#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_pool_follow(
    pool: *mut plsm_udp_pool,
    start_kind: u32,
    start_value: u64,
    live: u32,
    out_follow: *mut *mut plsm_udp_follow,
    out_err: *mut *mut plsm_error,
) -> i32 {
    if out_follow.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_follow is null"),
        );
    }
    if pool.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("pool is null"),
        );
    }
    let start = match start_kind {
        0 => crate::api::UdpStart::New,
        1 => crate::api::UdpStart::Last(start_value),
        2 => crate::api::UdpStart::Since(start_value),
        3 => crate::api::UdpStart::Sequence(start_value),
        _ => {
            return fail(
                out_err,
                Error::new(ErrorKind::Usage).with_message("invalid UDP follow start kind"),
            );
        }
    };
    let follow = match unsafe { &*pool }.pool.follow(crate::api::UdpFollowOptions {
        start,
        live: live != 0,
    }) {
        Ok(follow) => follow,
        Err(err) => return fail(out_err, err),
    };
    unsafe {
        *out_follow = Box::into_raw(Box::new(plsm_udp_follow {
            follow: Mutex::new(follow),
            cancelled: AtomicBool::new(false),
        }));
    }
    0
}

/// Returns 1 for an event, 0 when the wait expires, 2 when the stream ends,
/// 3 when a concurrent caller cancels the wait, or -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_follow_next(
    follow: *mut plsm_udp_follow,
    timeout_ms: u64,
    out_event: *mut plsm_udp_event,
    out_err: *mut *mut plsm_error,
) -> i32 {
    if out_event.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_event is null"),
        );
    }
    if follow.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("follow is null"),
        );
    }
    let follow = unsafe { &*follow };
    if follow.cancelled.load(Ordering::Acquire) {
        return 3;
    }
    let mut guard = match follow.follow.lock() {
        Ok(guard) => guard,
        Err(_) => {
            return fail(
                out_err,
                Error::new(ErrorKind::Internal).with_message("UDP follow lock failed"),
            );
        }
    };
    let Some(deadline) = Instant::now().checked_add(Duration::from_millis(timeout_ms)) else {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("UDP wait timeout is too large"),
        );
    };
    loop {
        if follow.cancelled.load(Ordering::Acquire) {
            return 3;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let wait = remaining.min(Duration::from_millis(50));
        match guard.next_event_timeout(wait) {
            Ok(Some(event)) => match write_udp_event(out_event, event) {
                Ok(()) => return 1,
                Err(err) => return fail(out_err, err),
            },
            Ok(None) if guard.is_finished() => return 2,
            Ok(None) if Instant::now() >= deadline => return 0,
            Ok(None) => continue,
            Err(err) => return fail(out_err, err),
        }
    }
}

/// Signal an in-progress bounded wait to return with status 3.
#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_follow_cancel(follow: *mut plsm_udp_follow) {
    if !follow.is_null() {
        unsafe { &*follow }.cancelled.store(true, Ordering::Release);
    }
}

/// Close the subscription and free the handle after active calls return.
#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_follow_close(follow: *mut plsm_udp_follow) {
    if follow.is_null() {
        return;
    }
    let follow = unsafe { Box::from_raw(follow) };
    follow.cancelled.store(true, Ordering::Release);
    if let Ok(mut inner) = follow.follow.lock() {
        inner.close();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_event_free(event: *mut plsm_udp_event) {
    if event.is_null() {
        return;
    }
    unsafe {
        let event = &mut *event;
        plsm_buf_free(&mut event.payload);
        *event = plsm_udp_event::default();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_event_message(
    event: *const plsm_udp_event,
    out_message: *mut plsm_buf,
    out_err: *mut *mut plsm_error,
) -> i32 {
    if event.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("event is null"),
        );
    }
    let event = unsafe { &*event };
    if event.kind != 1 {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("event is not a frame"),
        );
    }
    let payload = if event.payload.data.is_null() || event.payload.len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(event.payload.data, event.payload.len) }
    };
    let message = match crate::api::message_from_udp_payload(event.seq, event.timestamp_ns, payload)
    {
        Ok(message) => message,
        Err(err) => return fail(out_err, err),
    };
    if let Err(err) = write_message_buf(out_message, message) {
        return fail(out_err, err);
    }
    0
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

fn parse_stream_options(
    options: *const plsm_stream_options,
    out_err: *mut *mut plsm_error,
) -> Result<StreamOpenOptions, i32> {
    if options.is_null() {
        return Err(fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("stream options are null"),
        ));
    }
    let struct_size = unsafe { std::ptr::addr_of!((*options).struct_size).read() } as usize;
    let required_size = std::mem::size_of::<plsm_stream_options>();
    if struct_size < required_size {
        return Err(fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message(format!(
                "stream options are too small: got {struct_size} bytes, need at least {required_size}"
            )),
        ));
    }
    let options = unsafe { &*options };
    if options.reserved != 0 {
        return Err(fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("stream options reserved field must be zero"),
        ));
    }
    let gap_policy = match options.gap_policy {
        0 => GapPolicy::Continue,
        1 => GapPolicy::Error,
        _ => {
            return Err(fail(
                out_err,
                Error::new(ErrorKind::Usage).with_message("invalid gap policy"),
            ));
        }
    };
    let max_messages = if options.has_max != 0 {
        match usize::try_from(options.max_messages) {
            Ok(max) => Some(max),
            Err(_) => {
                return Err(fail(
                    out_err,
                    Error::new(ErrorKind::Usage)
                        .with_message("max_messages exceeds platform limits"),
                ));
            }
        }
    } else {
        None
    };
    Ok(StreamOpenOptions {
        since_seq: (options.has_since != 0).then_some(options.since_seq),
        max_messages,
        timeout: (options.has_timeout != 0).then(|| Duration::from_millis(options.timeout_ms)),
        gap_policy,
    })
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
) -> Result<&'a plsm_stream, i32> {
    if stream.is_null() {
        return Err(fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("stream is null"),
        ));
    }
    unsafe { Ok(&*stream) }
}

fn borrow_lite3_stream<'a>(
    stream: *mut plsm_lite3_stream,
    out_err: *mut *mut plsm_error,
) -> Result<&'a plsm_lite3_stream, i32> {
    if stream.is_null() {
        return Err(fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("stream is null"),
        ));
    }
    unsafe { Ok(&*stream) }
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

fn parse_utf8_cstr(input: *const c_char, name: &str) -> Result<String, Error> {
    if input.is_null() {
        return Err(Error::new(ErrorKind::Usage).with_message(format!("{name} is null")));
    }
    unsafe { CStr::from_ptr(input) }
        .to_str()
        .map(str::to_owned)
        .map_err(|err| {
            Error::new(ErrorKind::Usage)
                .with_message(format!("{name} is not valid UTF-8"))
                .with_source(err)
        })
}

fn write_udp_event(out: *mut plsm_udp_event, event: crate::api::UdpEvent) -> Result<(), Error> {
    if out.is_null() {
        return Err(Error::new(ErrorKind::Usage).with_message("out_event is null"));
    }
    let out = unsafe { &mut *out };
    *out = plsm_udp_event::default();
    match event {
        crate::api::UdpEvent::Frame(frame) => {
            out.kind = 1;
            out.seq = frame.seq;
            out.timestamp_ns = frame.timestamp_ns;
            out.payload = write_udp_payload(frame.payload);
        }
        crate::api::UdpEvent::Gap {
            first,
            last,
            reason,
        } => {
            out.kind = 2;
            out.first = first;
            out.last = last;
            out.reason = match reason {
                crate::api::UdpSkipReason::Unknown => 0,
                crate::api::UdpSkipReason::Oversized => 1,
                crate::api::UdpSkipReason::Retention => 2,
                crate::api::UdpSkipReason::Pressure => 3,
            };
        }
        crate::api::UdpEvent::Progress {
            start_seq,
            next_seq,
            end_seq,
            terminal,
            retention_gap,
        } => {
            out.kind = 3;
            out.start_seq = start_seq;
            out.next_seq = next_seq;
            if let Some(end_seq) = end_seq {
                out.end_seq = end_seq;
                out.has_end = 1;
            }
            out.terminal = u8::from(terminal);
            out.retention_gap = u8::from(retention_gap);
        }
    }
    Ok(())
}

fn write_udp_payload(payload: Vec<u8>) -> plsm_buf {
    if payload.is_empty() {
        return plsm_buf::default();
    }
    let mut data = payload.into_boxed_slice();
    let out = plsm_buf {
        data: data.as_mut_ptr(),
        len: data.len(),
    };
    std::mem::forget(data);
    out
}

fn parse_tags(tags: *const *const c_char, len: usize) -> Result<Vec<String>, Error> {
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

fn write_message_buf(
    out_message: *mut plsm_buf,
    message: crate::api::Message,
) -> Result<(), Error> {
    if out_message.is_null() {
        return Err(Error::new(ErrorKind::Usage).with_message("out_message is null"));
    }
    let wire = crate::interface_wire::MessageWire::new(
        message.seq,
        message.time,
        message.meta.tags,
        message.data,
    );
    let json_bytes = serde_json::to_vec(&wire).map_err(|err| {
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
    frame: crate::api::FrameRef,
) -> Result<(), Error> {
    if out_frame.is_null() {
        return Err(Error::new(ErrorKind::Usage).with_message("out_frame is null"));
    }
    unsafe {
        let out_frame = &mut *out_frame;
        let mut data = frame.payload.into_boxed_slice();
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
        ErrorKind::RetentionGap => 9,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, CString};

    fn parse_buf(buf: &plsm_buf) -> Value {
        let slice = unsafe { std::slice::from_raw_parts(buf.data, buf.len) };
        serde_json::from_slice(slice).expect("valid json")
    }

    #[test]
    fn streams_own_their_pool_and_keep_legacy_count_and_deadline_behavior() {
        let temp = tempfile::tempdir().unwrap();
        let mut storage =
            Pool::create(temp.path().join("stream.plasmite"), PoolOptions::new(65536)).unwrap();
        storage
            .append_json_now(
                &serde_json::json!({"value": 1}),
                &[],
                crate::api::Durability::Fast,
            )
            .unwrap();
        let pool = Box::into_raw(Box::new(plsm_pool { pool: storage }));
        let mut options = default_stream_options();
        options.has_max = 1;
        options.max_messages = 1;
        options.has_timeout = 1;
        options.timeout_ms = 1000;
        let mut json = ptr::null_mut();
        let mut raw = ptr::null_mut();
        let mut expired = ptr::null_mut();
        let mut err = ptr::null_mut();
        assert_eq!(plsm_stream_open_ex(pool, &options, &mut json, &mut err), 0);
        assert_eq!(
            plsm_lite3_stream_open_ex(pool, &options, &mut raw, &mut err),
            0
        );
        options.timeout_ms = 0;
        assert_eq!(
            plsm_stream_open_ex(pool, &options, &mut expired, &mut err),
            0
        );
        plsm_pool_free(pool);

        let mut buf = plsm_buf::default();
        assert_eq!(plsm_stream_next(expired, &mut buf, &mut err), 0);
        assert_eq!(plsm_stream_next(json, &mut buf, &mut err), 1);
        assert_eq!(parse_buf(&buf)["data"]["value"], 1);
        plsm_buf_free(&mut buf);
        assert_eq!(plsm_stream_next(json, &mut buf, &mut err), 0);
        let mut frame = plsm_lite3_frame {
            seq: 0,
            timestamp_ns: 0,
            flags: 0,
            payload: plsm_buf::default(),
        };
        assert_eq!(plsm_lite3_stream_next(raw, &mut frame, &mut err), 1);
        assert_eq!(frame.seq, 1);
        crate::api::lite3::validate_bytes(&take_lite3_payload(&frame)).unwrap();
        plsm_lite3_frame_free(&mut frame);
        assert_eq!(plsm_lite3_stream_next(raw, &mut frame, &mut err), 0);
        assert!(err.is_null());
        plsm_stream_free(expired);
        plsm_stream_free(json);
        plsm_lite3_stream_free(raw);
    }

    #[test]
    fn bounded_waits_retain_position_and_distinguish_expiry_completion_and_cancel() {
        let temp = tempfile::tempdir().unwrap();
        let storage =
            Pool::create(temp.path().join("wait.plasmite"), PoolOptions::new(65536)).unwrap();
        let pool = Box::into_raw(Box::new(plsm_pool { pool: storage }));
        let mut options = default_stream_options();
        options.has_max = 1;
        options.max_messages = 2;
        let mut json = ptr::null_mut();
        let mut raw = ptr::null_mut();
        let mut err = ptr::null_mut();
        assert_eq!(plsm_stream_open_ex(pool, &options, &mut json, &mut err), 0);
        assert_eq!(
            plsm_lite3_stream_open_ex(pool, &options, &mut raw, &mut err),
            0
        );
        let mut buf = plsm_buf::default();
        let mut frame = plsm_lite3_frame {
            seq: 0,
            timestamp_ns: 0,
            flags: 0,
            payload: plsm_buf::default(),
        };
        assert_eq!(plsm_stream_next_wait(json, 0, &mut buf, &mut err), 0);
        assert_eq!(plsm_lite3_stream_next_wait(raw, 1, &mut frame, &mut err), 0);
        for seq in 1..=2 {
            append_json_for_test(pool, &serde_json::json!({"seq":seq}), &mut err);
            assert_eq!(plsm_stream_next_wait(json, 0, &mut buf, &mut err), 1);
            assert_eq!(parse_buf(&buf)["seq"], seq);
            plsm_buf_free(&mut buf);
            assert_eq!(plsm_lite3_stream_next_wait(raw, 0, &mut frame, &mut err), 1);
            assert_eq!(frame.seq, seq);
            plsm_lite3_frame_free(&mut frame);
        }
        assert_eq!(plsm_stream_next_wait(json, 0, &mut buf, &mut err), 2);
        assert_eq!(plsm_lite3_stream_next_wait(raw, 0, &mut frame, &mut err), 2);
        plsm_stream_free(json);
        plsm_lite3_stream_free(raw);

        options.has_max = 0;
        options.has_since = 1;
        options.since_seq = 3;
        assert_eq!(plsm_stream_open_ex(pool, &options, &mut json, &mut err), 0);
        let address = json as usize;
        let reader = std::thread::spawn(move || {
            let mut buf = plsm_buf::default();
            let mut err = ptr::null_mut();
            let status =
                plsm_stream_next_wait(address as *mut plsm_stream, 5000, &mut buf, &mut err);
            assert!(err.is_null());
            status
        });
        plsm_stream_cancel(json);
        assert_eq!(reader.join().unwrap(), 3);
        assert_eq!(plsm_stream_next(json, &mut buf, &mut err), 0);
        plsm_stream_free(json);
        assert_eq!(
            plsm_lite3_stream_open_ex(pool, &options, &mut raw, &mut err),
            0
        );
        plsm_lite3_stream_cancel(raw);
        assert_eq!(plsm_lite3_stream_next_wait(raw, 1, &mut frame, &mut err), 3);
        plsm_lite3_stream_free(raw);
        plsm_pool_free(pool);
    }

    #[test]
    fn abi_smoke_udp_delivers_late_and_duplicate_frames() {
        use crate::udp_wire as wire;
        use std::net::UdpSocket;
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let endpoint = CString::new(format!("udp://{}", peer.local_addr().unwrap())).unwrap();
        let base = 1u64 << 53;
        let timestamp = 1_700_000_000_000_000_001u64;
        let payload =
            crate::api::lite3::encode_message(&["tag".into()], &serde_json::json!({"value":17}))
                .unwrap();
        let expected = payload.as_slice().to_vec();
        let worker = std::thread::spawn(move || {
            let mut buffer = [0; 4096];
            let (len, address) = peer.recv_from(&mut buffer).unwrap();
            let (op, id, _) = wire::parse(&buffer[..len]).unwrap();
            assert_eq!(op, wire::SUBSCRIBE);
            peer.send_to(
                &wire::control(
                    wire::CHALLENGE,
                    id,
                    &serde_json::json!({
                        "cookie": {"expires_ms":10_000, "mac":"test-cookie"}
                    }),
                )
                .unwrap(),
                address,
            )
            .unwrap();
            peer.recv_from(&mut buffer).unwrap();
            let send = |packet: Vec<u8>| {
                peer.send_to(&packet, address).unwrap();
            };
            send(wire::data(id, base + 10, timestamp, payload.as_slice()).unwrap());
            let progress = |terminal| {
                wire::control(
                    wire::PROGRESS,
                    id,
                    &wire::Progress {
                        start_seq: base + 10,
                        next_seq: base + 13,
                        end_seq: Some(base + 12),
                        lease_ms: wire::LEASE_MS,
                        terminal,
                        retention_gap: false,
                    },
                )
                .unwrap()
            };
            send(progress(false));
            send(
                wire::control(
                    wire::SKIP,
                    id,
                    &wire::Skip {
                        first: base + 11,
                        last: base + 11,
                        reason: wire::Reason::Pressure,
                    },
                )
                .unwrap(),
            );
            for seq in [base + 12, base + 11, base + 12] {
                send(wire::data(id, seq, timestamp, payload.as_slice()).unwrap());
            }
            send(progress(true));
        });
        let mut client = ptr::null_mut();
        let mut pool = ptr::null_mut();
        let mut follow = ptr::null_mut();
        let mut error = ptr::null_mut();
        let name = CString::new("sensors").unwrap();
        assert_eq!(
            plsm_udp_client_new(endpoint.as_ptr(), &mut client, &mut error),
            0
        );
        assert_eq!(
            plsm_udp_client_open_pool(client, name.as_ptr(), &mut pool, &mut error),
            0
        );
        assert_eq!(
            plsm_udp_pool_follow(pool, 1, 4, 0, &mut follow, &mut error),
            0
        );
        let mut sequences = Vec::new();
        let mut skips = Vec::new();
        loop {
            let mut event = plsm_udp_event::default();
            let status = plsm_udp_follow_next(follow, 1000, &mut event, &mut error);
            if status == 2 {
                break;
            }
            assert_eq!(status, 1);
            match event.kind {
                1 => {
                    sequences.push(event.seq);
                    assert_eq!(event.timestamp_ns, timestamp);
                    assert_eq!(
                        unsafe {
                            std::slice::from_raw_parts(event.payload.data, event.payload.len)
                        },
                        expected
                    );
                    let mut message = plsm_buf::default();
                    assert_eq!(plsm_udp_event_message(&event, &mut message, &mut error), 0);
                    assert_eq!(parse_buf(&message)["seq"], event.seq);
                    plsm_buf_free(&mut message);
                }
                2 => skips.push((event.first, event.last, event.reason)),
                3 => assert_eq!(event.next_seq, base + 13),
                _ => panic!("unexpected UDP event"),
            }
            plsm_udp_event_free(&mut event);
        }
        assert!(error.is_null());
        assert_eq!(sequences, [base + 10, base + 12, base + 11, base + 12]);
        assert_eq!(skips, [(base + 11, base + 11, 3)]);
        plsm_udp_follow_close(follow);
        plsm_udp_pool_free(pool);
        plsm_udp_client_free(client);
        worker.join().unwrap();
    }

    #[test]
    fn udp_event_decodes_unaligned_payload() {
        let data = serde_json::json!({"value": 17});
        let document = crate::api::lite3::encode_message(&["tag".into()], &data).unwrap();
        let mut bytes = vec![0; document.len() + 1];
        let offset = usize::from((bytes.as_ptr() as usize).is_multiple_of(4));
        let payload = &mut bytes[offset..offset + document.len()];
        payload.copy_from_slice(document.as_slice());
        let event = plsm_udp_event {
            kind: 1,
            seq: 42,
            payload: plsm_buf {
                data: payload.as_mut_ptr(),
                len: payload.len(),
            },
            ..Default::default()
        };
        let mut message = plsm_buf::default();
        let mut error = std::ptr::null_mut();
        assert_eq!(plsm_udp_event_message(&event, &mut message, &mut error), 0);
        let json = parse_buf(&message);
        plsm_buf_free(&mut message);
        assert_eq!(json["seq"], 42);
        assert_eq!(json["meta"]["tags"], serde_json::json!(["tag"]));
        assert_eq!(json["data"], data);
        assert_eq!(payload, document.as_slice());
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

    fn default_stream_options() -> plsm_stream_options {
        plsm_stream_options {
            struct_size: std::mem::size_of::<plsm_stream_options>() as u32,
            gap_policy: 0,
            since_seq: 0,
            max_messages: 0,
            timeout_ms: 0,
            has_since: 0,
            has_max: 0,
            has_timeout: 0,
            reserved: 0,
        }
    }

    fn append_json_for_test(pool: *mut plsm_pool, value: &Value, err: &mut *mut plsm_error) -> u64 {
        let bytes = serde_json::to_vec(value).expect("json");
        let mut out = plsm_buf {
            data: std::ptr::null_mut(),
            len: 0,
        };
        let rc = plsm_pool_append_json(
            pool,
            bytes.as_ptr(),
            bytes.len(),
            std::ptr::null(),
            0,
            0,
            &mut out,
            err,
        );
        assert_eq!(rc, 0, "append failed");
        let seq = parse_buf(&out)["seq"].as_u64().expect("seq");
        plsm_buf_free(&mut out);
        seq
    }

    fn fill_pool_past(pool: *mut plsm_pool, seq: u64, err: &mut *mut plsm_error) -> u64 {
        for n in 0..10_000_u64 {
            append_json_for_test(
                pool,
                &serde_json::json!({
                    "n": n,
                    "padding": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
                }),
                err,
            );
            let oldest = unsafe { &*pool }
                .pool
                .info()
                .expect("pool info")
                .bounds
                .oldest_seq
                .expect("oldest");
            if oldest > seq {
                return oldest;
            }
        }
        panic!("pool bounds did not advance beyond sequence {seq}");
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
            payload.as_bytes().as_ptr(),
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
    fn stream_open_ex_validates_versioned_options() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pool_dir = temp.path().join("pools");
        std::fs::create_dir_all(&pool_dir).expect("mkdir");
        let pool_dir_c = CString::new(pool_dir.to_string_lossy().as_ref()).expect("cstr");
        let mut client: *mut plsm_client = std::ptr::null_mut();
        let mut pool: *mut plsm_pool = std::ptr::null_mut();
        let mut stream: *mut plsm_stream = std::ptr::null_mut();
        let mut err: *mut plsm_error = std::ptr::null_mut();
        assert_eq!(
            plsm_client_new(pool_dir_c.as_ptr(), &mut client, &mut err),
            0
        );
        let pool_name = CString::new("stream-options").expect("cstr");
        assert_eq!(
            plsm_pool_create(client, pool_name.as_ptr(), 8192, &mut pool, &mut err),
            0
        );

        assert_eq!(
            plsm_stream_open_ex(pool, std::ptr::null(), &mut stream, &mut err),
            -1
        );
        assert_eq!(take_error(err).0, error_kind_code(ErrorKind::Usage));

        let mut options = default_stream_options();
        options.struct_size -= 1;
        assert_eq!(
            plsm_stream_open_ex(pool, &options, &mut stream, &mut err),
            -1
        );
        assert_eq!(take_error(err).0, error_kind_code(ErrorKind::Usage));

        let mut options = default_stream_options();
        options.reserved = 1;
        assert_eq!(
            plsm_stream_open_ex(pool, &options, &mut stream, &mut err),
            -1
        );
        assert_eq!(take_error(err).0, error_kind_code(ErrorKind::Usage));

        let mut options = default_stream_options();
        options.gap_policy = 99;
        assert_eq!(
            plsm_stream_open_ex(pool, &options, &mut stream, &mut err),
            -1
        );
        assert_eq!(take_error(err).0, error_kind_code(ErrorKind::Usage));

        #[repr(C)]
        struct FutureOptions {
            v1: plsm_stream_options,
            future: u64,
        }
        let mut future = FutureOptions {
            v1: default_stream_options(),
            future: 42,
        };
        future.v1.struct_size = std::mem::size_of::<FutureOptions>() as u32;
        assert_eq!(
            plsm_stream_open_ex(pool, &future.v1, &mut stream, &mut err),
            0
        );
        assert_eq!(future.future, 42);
        plsm_stream_free(stream);

        plsm_pool_free(pool);
        plsm_client_free(client);
    }

    #[test]
    fn stream_open_ex_reports_retention_gaps_for_json_and_lite3() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pool_dir = temp.path().join("pools");
        std::fs::create_dir_all(&pool_dir).expect("mkdir");
        let pool_dir_c = CString::new(pool_dir.to_string_lossy().as_ref()).expect("cstr");
        let mut client: *mut plsm_client = std::ptr::null_mut();
        let mut pool: *mut plsm_pool = std::ptr::null_mut();
        let mut err: *mut plsm_error = std::ptr::null_mut();
        assert_eq!(
            plsm_client_new(pool_dir_c.as_ptr(), &mut client, &mut err),
            0
        );
        let pool_name = CString::new("stream-gap").expect("cstr");
        assert_eq!(
            plsm_pool_create(client, pool_name.as_ptr(), 8192, &mut pool, &mut err),
            0
        );
        let first = append_json_for_test(pool, &serde_json::json!({"n": 1}), &mut err);
        let oldest = fill_pool_past(pool, first, &mut err);

        let mut options = default_stream_options();
        options.gap_policy = 1;
        options.since_seq = first;
        options.has_since = 1;

        let mut stream: *mut plsm_stream = std::ptr::null_mut();
        assert_eq!(
            plsm_stream_open_ex(pool, &options, &mut stream, &mut err),
            0
        );
        let mut out = plsm_buf {
            data: std::ptr::null_mut(),
            len: 0,
        };
        assert_eq!(plsm_stream_next(stream, &mut out, &mut err), -1);
        let (kind, message, _path, seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::RetentionGap));
        assert_eq!(seq, Some(first));
        assert!(message.contains("the next available message is"));
        assert_eq!(plsm_stream_next(stream, &mut out, &mut err), 0);
        plsm_stream_free(stream);

        let mut lite3_stream: *mut plsm_lite3_stream = std::ptr::null_mut();
        assert_eq!(
            plsm_lite3_stream_open_ex(pool, &options, &mut lite3_stream, &mut err),
            0
        );
        let mut frame = plsm_lite3_frame {
            seq: 0,
            timestamp_ns: 0,
            flags: 0,
            payload: plsm_buf {
                data: std::ptr::null_mut(),
                len: 0,
            },
        };
        assert_eq!(
            plsm_lite3_stream_next(lite3_stream, &mut frame, &mut err),
            -1
        );
        let (kind, _message, _path, seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::RetentionGap));
        assert_eq!(seq, Some(first));
        plsm_lite3_stream_free(lite3_stream);

        options.gap_policy = 0;
        let mut continuing: *mut plsm_stream = std::ptr::null_mut();
        assert_eq!(
            plsm_stream_open_ex(pool, &options, &mut continuing, &mut err),
            0
        );
        assert_eq!(plsm_stream_next(continuing, &mut out, &mut err), 1);
        assert!(
            parse_buf(&out)["seq"]
                .as_u64()
                .is_some_and(|seq| seq >= oldest)
        );
        plsm_buf_free(&mut out);
        plsm_stream_free(continuing);

        plsm_pool_free(pool);
        plsm_client_free(client);
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
