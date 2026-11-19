//! Purpose: C ABI bridge for bindings (libplasmite).
//! Exports: C-callable client/pool/stream functions and buffer/error helpers.
//! Role: Version 2 C ABI for language bindings.
//!
//! ABI stability: additive-only within a major version — no field removals,
//! no struct reordering, no enum discriminant changes.  See `include/plasmite.h`
//! for the full stability contract, ownership rules, and linking notes.
//!
//! Invariants: JSON bytes in/out; Lite3 bytes for fast paths; explicit free functions.
//! Invariants: Error kinds map 1:1 with core error kinds.
#![allow(clippy::result_large_err)]

use crate::abi_remote::{PoolHandle, RemoteFrames};
use crate::api::{Feed, LocalClient, PoolOptions, PoolRef, Read, Since, TailOutcome, TailState};
use crate::core::error::{Error, ErrorKind};
use crate::core::notify::CancelSignal;
use crate::core::pool::{Durability, Pool};
#[cfg(test)]
use serde_json::Value;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[unsafe(no_mangle)]
pub extern "C" fn plsm_abi_major() -> u32 {
    2
}

#[repr(C)]
pub struct plsm_client {
    client: LocalClient,
}

#[repr(C)]
pub struct plsm_pool {
    pub(crate) pool: PoolHandle,
}

#[repr(C)]
pub struct plsm_stream {
    state: Mutex<StreamState>,
    cancel: Arc<CancelSignal>,
}

#[repr(C)]
pub struct plsm_lite3_stream {
    state: Mutex<StreamState>,
    cancel: Arc<CancelSignal>,
}

enum StreamState {
    Local { pool: Box<Pool>, state: TailState },
    Remote(RemoteFrames),
}

impl StreamState {
    fn next<T>(
        &mut self,
        wait: Option<Duration>,
        select: impl Fn(crate::api::FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<TailOutcome<T>, Error> {
        match self {
            Self::Local { pool, state } => state.next_wait(pool, wait, select),
            Self::Remote(reader) => reader.next(wait, usize::MAX, select),
        }
    }
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
    follow: Mutex<UdpFollower>,
    cancelled: AtomicBool,
}

enum UdpFollower {
    Raw(crate::api::UdpFollow),
    Views(crate::api::UdpViewFollow),
}

#[repr(C)]
#[derive(Default)]
pub struct plsm_buf {
    data: *mut u8,
    len: usize,
}

#[repr(C)]
#[derive(Default)]
pub struct plsm_feed_receipt {
    seq: u64,
    time: u64,
}

#[repr(C)]
pub struct plsm_lite3_frame {
    seq: u64,
    time: u64,
    flags: u32,
    payload: plsm_buf,
}

#[repr(C)]
#[derive(Default)]
pub struct plsm_udp_event {
    kind: u32,
    reason: u32,
    seq: u64,
    time: u64,
    first: u64,
    last: u64,
    start_seq: u64,
    next_seq: u64,
    end_seq: u64,
    has_end: u8,
    terminal: u8,
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
    missing: u64,
    next_seq: u64,
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
        crate::api::DEFAULT_POOL_SIZE
    } else {
        size_bytes
    };
    let options = PoolOptions::new(size);
    let result = match &pool_ref {
        PoolRef::Uri(uri) => crate::api::RemoteClient::for_pool_uri(uri)
            .and_then(|client| client.create_pool(&pool_ref, options))
            .map(PoolHandle::Remote),
        _ => client
            .client
            .create_pool(&pool_ref, options)
            .map(PoolHandle::from),
    };
    let pool = match result {
        Ok(pool) => pool,
        Err(err) => return fail(out_err, err),
    };
    let handle = Box::new(plsm_pool { pool });
    unsafe {
        *out_pool = Box::into_raw(handle);
    }
    0
}

fn open_pool_handle(
    client: *mut plsm_client,
    pool_ref: *const c_char,
    create: bool,
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
    if out_pool.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("out_pool is null"),
        );
    }
    let result = match &pool_ref {
        PoolRef::Uri(uri) => crate::api::RemoteClient::for_pool_uri(uri)
            .and_then(|client| client.open_pool(&pool_ref, create))
            .map(PoolHandle::Remote),
        _ => client
            .client
            .open_pool(&pool_ref, create)
            .map(PoolHandle::from),
    };
    let pool = match result {
        Ok(pool) => pool,
        Err(err) => return fail(out_err, err),
    };
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

fn stream_wait(timeout_ms: u64) -> Option<Duration> {
    (timeout_ms != u64::MAX).then(|| Duration::from_millis(timeout_ms))
}

fn read_stream(
    stream: &plsm_stream,
    wait: Option<Duration>,
) -> Result<TailOutcome<Vec<u8>>, Error> {
    let mut state = stream
        .state
        .lock()
        .map_err(|_| Error::new(ErrorKind::Internal).with_message("stream state is unavailable"))?;
    state.next(wait, crate::api::select_frame_json)
}

fn read_lite3_stream(
    stream: &plsm_lite3_stream,
    wait: Option<Duration>,
) -> Result<TailOutcome<crate::api::FrameRef>, Error> {
    let mut state = stream
        .state
        .lock()
        .map_err(|_| Error::new(ErrorKind::Internal).with_message("stream state is unavailable"))?;
    state.next(wait, crate::api::select_lite3_frame)
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
    match read_stream(stream, stream_wait(timeout_ms)) {
        Ok(TailOutcome::Item(message)) => match write_json_buf(out_message, message) {
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
    match read_lite3_stream(stream, stream_wait(timeout_ms)) {
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
        stream.cancel.cancel();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_lite3_stream_cancel(stream: *mut plsm_lite3_stream) {
    if let Some(stream) = unsafe { stream.as_ref() } {
        stream.cancel.cancel();
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
pub extern "C" fn plsm_udp_pool_feed_lite3(
    pool: *mut plsm_udp_pool,
    payload: *const u8,
    payload_len: usize,
    options: *const plsm_feed_options,
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
    let result =
        parse_feed(options).and_then(|feed| unsafe { &*pool }.pool.feed_lite3(payload, feed));
    match result {
        Ok(()) => 0,
        Err(err) => fail(out_err, err),
    }
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
    let UdpFollower::Raw(reader) = &mut *guard else {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("use next_view for an owned UDP follow"),
        );
    };
    match udp_wait(&follow.cancelled, timeout_ms, |wait| {
        Ok((reader.next_event_timeout(wait)?, reader.is_finished()))
    }) {
        Ok(TailOutcome::Item(event)) => match write_udp_event(out_event, event) {
            Ok(()) => 1,
            Err(err) => fail(out_err, err),
        },
        Ok(TailOutcome::WaitExpired) => 0,
        Ok(TailOutcome::Finished) => 2,
        Ok(TailOutcome::Cancelled) => 3,
        Err(err) => fail(out_err, err),
    }
}

fn udp_wait<T>(
    cancelled: &AtomicBool,
    timeout_ms: u64,
    mut next: impl FnMut(Duration) -> Result<(Option<T>, bool), Error>,
) -> Result<TailOutcome<T>, Error> {
    let deadline = stream_wait(timeout_ms)
        .map(|wait| {
            Instant::now()
                .checked_add(wait)
                .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("UDP wait is too large"))
        })
        .transpose()?;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(TailOutcome::Cancelled);
        }
        let interval = Duration::from_millis(50);
        let wait = deadline.map_or(interval, |deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .min(interval)
        });
        match next(wait)? {
            (Some(event), _) => return Ok(TailOutcome::Item(event)),
            (None, true) => return Ok(TailOutcome::Finished),
            (None, false) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
                return Ok(TailOutcome::WaitExpired);
            }
            _ => continue,
        }
    }
}

/// DATA returns an independent view owner; control events return a null owner.
#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_follow_next_view(
    follow: *mut plsm_udp_follow,
    timeout_ms: u64,
    out_event: *mut plsm_udp_event,
    out_owner: *mut *const crate::abi_views::plsm_message_view,
    out_err: *mut *mut plsm_error,
) -> i32 {
    if follow.is_null() || out_event.is_null() || out_owner.is_null() {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("invalid UDP view argument"),
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
    let UdpFollower::Views(reader) = &mut *guard else {
        return fail(
            out_err,
            Error::new(ErrorKind::Usage).with_message("owned UDP reads require follow_views"),
        );
    };
    match udp_wait(&follow.cancelled, timeout_ms, |wait| {
        Ok((reader.next_event_timeout(wait)?, reader.is_finished()))
    }) {
        Ok(TailOutcome::Item(event)) => {
            use crate::api::{UdpEvent, UdpStreamEvent};
            let control = match event {
                UdpStreamEvent::Frame(view) => {
                    let (seq, time) = (view.seq, view.time);
                    if let Err(err) = crate::abi_views::publish(view, out_owner) {
                        return fail(out_err, err);
                    }
                    unsafe {
                        *out_event = plsm_udp_event {
                            kind: 1,
                            seq,
                            time,
                            ..Default::default()
                        };
                    }
                    return 1;
                }
                UdpStreamEvent::Skip {
                    first,
                    last,
                    reason,
                } => UdpEvent::Skip {
                    first,
                    last,
                    reason,
                },
                UdpStreamEvent::Progress {
                    start_seq,
                    next_seq,
                    end_seq,
                    terminal,
                } => UdpEvent::Progress {
                    start_seq,
                    next_seq,
                    end_seq,
                    terminal,
                },
            };
            match write_udp_event(out_event, control) {
                Ok(()) => {
                    unsafe {
                        *out_owner = ptr::null();
                    }
                    1
                }
                Err(err) => fail(out_err, err),
            }
        }
        Ok(TailOutcome::WaitExpired) => 0,
        Ok(TailOutcome::Finished) => 2,
        Ok(TailOutcome::Cancelled) => 3,
        Err(err) => fail(out_err, err),
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
        match &mut *inner {
            UdpFollower::Raw(reader) => reader.close(),
            UdpFollower::Views(reader) => reader.close(),
        }
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
    let message = match crate::api::message_from_udp_payload(event.seq, event.time, payload) {
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
        frame.time = 0;
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

pub(crate) fn borrow_pool<'a>(
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
        return Ok(PoolRef::uri(raw));
    }
    #[cfg(windows)]
    if !std::path::Path::new(raw).has_root()
        && matches!(
            std::path::Path::new(raw).components().next(),
            Some(std::path::Component::Prefix(_))
        )
    {
        return Err(fail(out_err, Error::new(ErrorKind::Usage).with_message("drive-relative pool paths are not supported; use an absolute or explicit relative path")));
    }
    if raw.chars().any(std::path::is_separator) {
        Ok(PoolRef::path(raw))
    } else {
        Ok(PoolRef::name(raw))
    }
}

// Callers retain these C input bytes through the ABI operation.
unsafe fn feed_input_bytes<'a>(
    bytes: *const u8,
    len: usize,
    name: &str,
) -> Result<&'a [u8], Error> {
    if bytes.is_null() {
        return Err(Error::new(ErrorKind::Usage).with_message(format!("{name} is null")));
    }
    Ok(unsafe { std::slice::from_raw_parts(bytes, len) })
}

// Lite3 validates aligned nodes. A public byte span may start at any address.
pub(crate) fn with_aligned_payload<T>(payload: &[u8], use_payload: impl FnOnce(&[u8]) -> T) -> T {
    if payload.as_ptr().align_offset(4) == 0 {
        return use_payload(payload);
    }
    let mut words = vec![0u32; payload.len().div_ceil(4)];
    unsafe {
        std::ptr::copy_nonoverlapping(payload.as_ptr(), words.as_mut_ptr().cast(), payload.len());
        use_payload(std::slice::from_raw_parts(
            words.as_ptr().cast(),
            payload.len(),
        ))
    }
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
            out.time = frame.time;
            out.payload = write_udp_payload(frame.payload);
        }
        crate::api::UdpEvent::Skip {
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
        } => {
            out.kind = 3;
            out.start_seq = start_seq;
            out.next_seq = next_seq;
            if let Some(end_seq) = end_seq {
                out.end_seq = end_seq;
                out.has_end = 1;
            }
            out.terminal = u8::from(terminal);
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

fn write_message_buf(
    out_message: *mut plsm_buf,
    message: crate::api::Message,
) -> Result<(), Error> {
    if out_message.is_null() {
        return Err(Error::new(ErrorKind::Usage).with_message("out_message is null"));
    }
    let wire = crate::interface_wire::MessageWire::new(
        message.seq,
        message.time_rfc3339(),
        message.meta.tags,
        message.data,
    );
    let json_bytes = serde_json::to_vec(&wire).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("failed to serialize message")
            .with_source(err)
    })?;
    write_json_buf(out_message, json_bytes)
}

pub(crate) fn write_json_buf(out_message: *mut plsm_buf, json_bytes: Vec<u8>) -> Result<(), Error> {
    if out_message.is_null() {
        return Err(Error::new(ErrorKind::Usage).with_message("out_message is null"));
    }
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
        out_frame.time = frame.time;
        out_frame.flags = frame.flags;
        std::mem::forget(data);
    }
    Ok(())
}

pub(crate) fn fail(out_err: *mut *mut plsm_error, err: Error) -> i32 {
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
        missing: err.missing().unwrap_or(0),
        next_seq: err.next_seq().unwrap_or(0),
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

#[repr(C)]
pub struct plsm_read_options {
    struct_size: u32,
    start_kind: u32,
    start_value: u64,
    tags: *const batches::ViewStreamText,
    tags_len: usize,
}

#[repr(C)]
pub struct plsm_feed_options {
    pub(super) struct_size: u32,
    pub(super) durability: u32,
    pub(super) tags: *const batches::ViewStreamText,
    pub(super) tags_len: usize,
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_open(
    client: *mut plsm_client,
    pool_ref: *const c_char,
    create: u32,
    out: *mut *mut plsm_pool,
    error: *mut *mut plsm_error,
) -> i32 {
    if create > 1 {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("create must be zero or one"),
        );
    }
    open_pool_handle(client, pool_ref, create == 1, out, error)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_info(
    pool: *mut plsm_pool,
    out: *mut plsm_buf,
    error: *mut *mut plsm_error,
) -> i32 {
    let pool = match borrow_pool(pool, error) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let result = pool
        .pool
        .info()
        .and_then(|info| write_wire(out, &pool_info_wire(&info)));
    result.map_or_else(|err| fail(error, err), |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_list(
    client: *mut plsm_client,
    out: *mut plsm_buf,
    error: *mut *mut plsm_error,
) -> i32 {
    let client = match borrow_client(client, error) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let result = client
        .client
        .list_pools()
        .and_then(|pools| write_wire(out, &pools.iter().map(pool_info_wire).collect::<Vec<_>>()));
    result.map_or_else(|err| fail(error, err), |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_delete(
    client: *mut plsm_client,
    pool_ref: *const c_char,
    error: *mut *mut plsm_error,
) -> i32 {
    let client = match borrow_client(client, error) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let pool_ref = match parse_pool_ref(pool_ref, error) {
        Ok(pool_ref) => pool_ref,
        Err(code) => return code,
    };
    let result = match &pool_ref {
        PoolRef::Uri(uri) => crate::api::RemoteClient::for_pool_uri(uri)
            .and_then(|client| client.delete_pool(&pool_ref)),
        _ => client.client.delete_pool(&pool_ref),
    };
    result.map_or_else(|err| fail(error, err), |()| 0)
}

fn pool_info_wire(info: &crate::api::PoolInfo) -> crate::interface_wire::PoolInfoWire {
    crate::interface_wire::pool_info_wire!(info.path.display(), info)
}

fn write_wire(out: *mut plsm_buf, value: &impl serde::Serialize) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("failed to serialize response")
            .with_source(err)
    })?;
    write_json_buf(out, bytes)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_feed_json(
    pool: *mut plsm_pool,
    bytes: *const u8,
    len: usize,
    options: *const plsm_feed_options,
    out: *mut plsm_buf,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_message is null"),
        );
    }
    let pool = match borrow_pool(pool, error) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let result = (|| {
        let parsed = crate::core::lite3::ParsedJson::parse(unsafe {
            feed_input_bytes(bytes, len, "json_bytes")
        }?)?;
        let (durability, tags) = feed_arguments(options)?;
        let tags = tags.unwrap_or_default();
        let payload = parsed.encode_message(&tags)?;
        let doc = payload.as_doc();
        let data =
            crate::core::lite3::JsonValue::new(doc, doc.message_data_offset(ErrorKind::Corrupt)?)?;
        let crate::api::FeedReceipt { seq, time } =
            pool.pool.append_prepared(payload.as_slice(), durability)?;
        let wire =
            crate::interface_wire::MessageWire::new(seq, crate::api::format_ts(time)?, &tags, data);
        if len < 4096 {
            return write_wire(out, &wire);
        }
        // Use input size as a hint without reserving space for arbitrarily
        // large amounts of whitespace that never reach the receipt.
        let bytes = wire.to_json_bytes(len.min(64 * 1024)).map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("failed to serialize response")
                .with_source(err)
        })?;
        write_json_buf(out, bytes)
    })();
    result.map_or_else(|err| fail(error, err), |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_feed_lite3(
    pool: *mut plsm_pool,
    bytes: *const u8,
    len: usize,
    options: *const plsm_feed_options,
    out: *mut plsm_buf,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_message is null"),
        );
    }
    let pool = match borrow_pool(pool, error) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let result = (|| {
        let message = with_aligned_payload(
            unsafe { feed_input_bytes(bytes, len, "lite3_bytes") }?,
            |payload| pool.pool.feed_lite3(payload, parse_feed(options)?),
        )?;
        write_message_buf(out, message)
    })();
    result.map_or_else(|err| fail(error, err), |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_feed_json_receipt(
    pool: *mut plsm_pool,
    bytes: *const u8,
    len: usize,
    options: *const plsm_feed_options,
    out: *mut plsm_feed_receipt,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_receipt is null"),
        );
    }
    let pool = match borrow_pool(pool, error) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let result = (|| {
        let parsed = crate::core::lite3::ParsedJson::parse(unsafe {
            feed_input_bytes(bytes, len, "json_bytes")
        }?)?;
        let (durability, tags) = feed_arguments(options)?;
        let payload = parsed.encode_message(&tags.unwrap_or_default())?;
        let crate::api::FeedReceipt { seq, time } =
            pool.pool.append_prepared(payload.as_slice(), durability)?;
        unsafe { out.write(plsm_feed_receipt { seq, time }) };
        Ok(())
    })();
    result.map_or_else(|err| fail(error, err), |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_feed_lite3_receipt(
    pool: *mut plsm_pool,
    bytes: *const u8,
    len: usize,
    options: *const plsm_feed_options,
    out: *mut plsm_feed_receipt,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_receipt is null"),
        );
    }
    let pool = match borrow_pool(pool, error) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let result = (|| {
        let receipt = with_aligned_payload(
            unsafe { feed_input_bytes(bytes, len, "lite3_bytes") }?,
            |payload| pool.pool.feed_lite3_receipt(payload, parse_feed(options)?),
        )?;
        unsafe {
            out.write(plsm_feed_receipt {
                seq: receipt.seq,
                time: receipt.time,
            })
        };
        Ok(())
    })();
    result.map_or_else(|err| fail(error, err), |()| 0)
}

pub(crate) fn parse_feed(options: *const plsm_feed_options) -> Result<Feed, Error> {
    let (durability, tags) = feed_arguments(options)?;
    let feed = Feed::new().durability(durability);
    Ok(match tags {
        Some(tags) => feed.tags(tags),
        None => feed,
    })
}

pub(crate) fn feed_arguments<'a>(
    options: *const plsm_feed_options,
) -> Result<(Durability, Option<Vec<&'a str>>), Error> {
    if options.is_null() {
        return Ok((Durability::Fast, None));
    }
    let size = unsafe { ptr::addr_of!((*options).struct_size).read() } as usize;
    if size < std::mem::size_of::<plsm_feed_options>() {
        return Err(Error::new(ErrorKind::Usage).with_message("feed options are too small"));
    }
    let options = unsafe { &*options };
    let durability = match options.durability {
        0 => Durability::Fast,
        1 => Durability::Flush,
        _ => {
            return Err(
                Error::new(ErrorKind::Usage).with_message("durability must be fast or flush")
            );
        }
    };
    let tags = if options.tags_len == 0 {
        None
    } else {
        Some(unsafe { batches::tag_spans(options.tags, options.tags_len) }?)
    };
    Ok((durability, tags))
}

fn parse_read(options: *const plsm_read_options) -> Result<Read, Error> {
    if options.is_null() {
        return Ok(Read::new());
    }
    let size = unsafe { ptr::addr_of!((*options).struct_size).read() } as usize;
    if size < std::mem::size_of::<plsm_read_options>() {
        return Err(Error::new(ErrorKind::Usage).with_message("read options are too small"));
    }
    let options = unsafe { &*options };
    let read = match options.start_kind {
        0 => Read::new(),
        1 => Read::since(Since::Oldest),
        2 => Read::since(Since::Seq(options.start_value)),
        3 => Read::since(Since::At(options.start_value)),
        4 => Read::since(Since::Ago(Duration::from_nanos(options.start_value))),
        5 => Read::last(options.start_value),
        _ => return Err(Error::new(ErrorKind::Usage).with_message("invalid read start kind")),
    };
    Ok(read.tags(unsafe { batches::tag_spans(options.tags, options.tags_len) }?))
}

fn read_state(
    handle: *mut plsm_pool,
    options: *const plsm_read_options,
    live: bool,
    error: *mut *mut plsm_error,
) -> Result<(StreamState, Arc<CancelSignal>), i32> {
    let handle = borrow_pool(handle, error)?;
    let cancel = Arc::new(CancelSignal::default());
    let options = parse_read(options).map_err(|err| fail(error, err))?;
    let state = (|| match &handle.pool {
        PoolHandle::Local(source) => {
            let pool = Box::new(source.reopen()?);
            #[cfg(not(any(target_os = "linux", windows)))]
            let options = options.cancel(cancel.flag());
            let state = pool.read_state(options, live)?;
            #[cfg(any(target_os = "linux", windows))]
            let state = state.with_cancel_signal(cancel.clone());
            Ok(StreamState::Local { pool, state })
        }
        PoolHandle::Remote(pool) => {
            let options = options.cancel(cancel.flag());
            let reader = if live {
                pool.follow_lite3(options)?
            } else {
                pool.fetch_lite3(options)?
            };
            RemoteFrames::start(reader, cancel.flag()).map(StreamState::Remote)
        }
    })()
    .map_err(|err| fail(error, err))?;
    Ok((state, cancel))
}

fn open_text(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    live: bool,
    out: *mut *mut plsm_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_stream is null"),
        );
    }
    let (state, cancel) = match read_state(pool, options, live, error) {
        Ok(values) => values,
        Err(code) => return code,
    };
    let stream = Box::new(plsm_stream {
        state: Mutex::new(state),
        cancel,
    });
    unsafe {
        *out = Box::into_raw(stream);
    }
    0
}

fn open_binary(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    live: bool,
    out: *mut *mut plsm_lite3_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_stream is null"),
        );
    }
    let (state, cancel) = match read_state(pool, options, live, error) {
        Ok(values) => values,
        Err(code) => return code,
    };
    let stream = Box::new(plsm_lite3_stream {
        state: Mutex::new(state),
        cancel,
    });
    unsafe {
        *out = Box::into_raw(stream);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_follow(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    out: *mut *mut plsm_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    open_text(pool, options, true, out, error)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_fetch(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    out: *mut *mut plsm_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    open_text(pool, options, false, out, error)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_follow_lite3(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    out: *mut *mut plsm_lite3_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    open_binary(pool, options, true, out, error)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_fetch_lite3(
    pool: *mut plsm_pool,
    options: *const plsm_read_options,
    out: *mut *mut plsm_lite3_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    open_binary(pool, options, false, out, error)
}

// A successful UDP feed only confirms local socket admission.
#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_pool_feed_json(
    pool: *mut plsm_udp_pool,
    bytes: *const u8,
    len: usize,
    options: *const plsm_feed_options,
    error: *mut *mut plsm_error,
) -> i32 {
    let result = (|| {
        let pool = unsafe { pool.as_ref() }
            .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("UDP pool is null"))?;
        let data: serde_json::Value =
            serde_json::from_slice(unsafe { feed_input_bytes(bytes, len, "json_bytes") }?)
                .map_err(|err| {
                    Error::new(ErrorKind::Usage)
                        .with_message("invalid JSON object")
                        .with_source(err)
                })?;
        pool.pool.feed(&data, parse_feed(options)?)
    })();
    result.map_or_else(|err| fail(error, err), |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_udp_pool_read(
    pool: *mut plsm_udp_pool,
    options: *const plsm_read_options,
    live: u32,
    views: u32,
    out: *mut *mut plsm_udp_follow,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() || live > 1 || views > 1 {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("invalid UDP reader arguments"),
        );
    }
    let result = (|| {
        let handle = unsafe { pool.as_ref() }
            .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("UDP pool is null"))?;
        let read = parse_read(options)?;
        let follow = match (live == 1, views == 1) {
            (true, false) => UdpFollower::Raw(handle.pool.follow(read)?),
            (false, false) => UdpFollower::Raw(handle.pool.fetch(read)?),
            (true, true) => UdpFollower::Views(handle.pool.follow_view(read)?),
            (false, true) => UdpFollower::Views(handle.pool.fetch_view(read)?),
        };
        unsafe {
            *out = Box::into_raw(Box::new(plsm_udp_follow {
                follow: Mutex::new(follow),
                cancelled: AtomicBool::new(false),
            }));
        }
        Ok(())
    })();
    result.map_or_else(|err| fail(error, err), |()| 0)
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
    fn abi_readers_own_their_pool_and_wait_until_cancelled() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wait.plasmite");
        let mut writer = Pool::create(&path, PoolOptions::new(65536)).unwrap();
        let pool = Box::into_raw(Box::new(plsm_pool {
            pool: Pool::open(&path).unwrap().into(),
        }));
        let mut json = ptr::null_mut();
        let mut raw = ptr::null_mut();
        let mut err = ptr::null_mut();
        assert_eq!(plsm_pool_follow(pool, ptr::null(), &mut json, &mut err), 0);
        assert_eq!(
            plsm_pool_follow_lite3(pool, ptr::null(), &mut raw, &mut err),
            0
        );
        plsm_pool_free(pool);
        let mut buf = plsm_buf::default();
        let mut frame = plsm_lite3_frame {
            seq: 0,
            time: 0,
            flags: 0,
            payload: plsm_buf::default(),
        };
        assert_eq!(plsm_stream_next_wait(json, 0, &mut buf, &mut err), 0);
        assert_eq!(plsm_lite3_stream_next_wait(raw, 1, &mut frame, &mut err), 0);
        writer
            .feed(&serde_json::json!({"ready":true}), Default::default())
            .unwrap();
        assert_eq!(plsm_stream_next_wait(json, u64::MAX, &mut buf, &mut err), 1);
        assert_eq!(parse_buf(&buf)["seq"], 1);
        plsm_buf_free(&mut buf);
        assert_eq!(
            plsm_lite3_stream_next_wait(raw, u64::MAX, &mut frame, &mut err),
            1
        );
        assert_eq!(frame.seq, 1);
        plsm_lite3_frame_free(&mut frame);
        let address = json as usize;
        let reader = std::thread::spawn(move || {
            let mut buf = plsm_buf::default();
            let mut err = ptr::null_mut();
            let status =
                plsm_stream_next_wait(address as *mut plsm_stream, u64::MAX, &mut buf, &mut err);
            assert!(err.is_null());
            status
        });
        plsm_stream_cancel(json);
        assert_eq!(reader.join().unwrap(), 3);
        plsm_lite3_stream_cancel(raw);
        assert_eq!(
            plsm_lite3_stream_next_wait(raw, u64::MAX, &mut frame, &mut err),
            3
        );
        plsm_stream_free(json);
        plsm_lite3_stream_free(raw);
    }

    #[test]
    fn abi_smoke_udp_delivers_late_and_duplicate_frames() {
        check_udp_follow(false);
        check_udp_follow(true);
    }

    fn check_udp_follow(views: bool) {
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
            plsm_udp_pool_read(
                pool,
                ptr::null(),
                0,
                u32::from(views),
                &mut follow,
                &mut error,
            ),
            0
        );
        let mut sequences = Vec::new();
        let mut skips = Vec::new();
        let mut retained = Vec::new();
        loop {
            let mut event = plsm_udp_event::default();
            let mut owner = ptr::null();
            let status = if views {
                plsm_udp_follow_next_view(follow, 1000, &mut event, &mut owner, &mut error)
            } else {
                plsm_udp_follow_next(follow, 1000, &mut event, &mut error)
            };
            if status == 2 {
                break;
            }
            assert_eq!(status, 1);
            match event.kind {
                1 => {
                    sequences.push(event.seq);
                    assert_eq!(event.time, timestamp);
                    if views {
                        assert!(event.payload.data.is_null());
                        assert_eq!(event.payload.len, 0);
                        assert_eq!(unsafe { crate::abi_views::plsm_view_seq(owner) }, event.seq);
                        retained.push(owner);
                        plsm_udp_event_free(&mut event);
                        continue;
                    }
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
                2 => {
                    assert!(owner.is_null());
                    skips.push((event.first, event.last, event.reason));
                }
                3 => {
                    assert!(owner.is_null());
                    assert_eq!(event.next_seq, base + 13);
                }
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
        for owner in retained {
            let mut len = 0;
            let bytes = unsafe { crate::abi_views::plsm_view_bytes(owner, &mut len) };
            assert_eq!(unsafe { std::slice::from_raw_parts(bytes, len) }, expected);
            assert_eq!(
                unsafe { crate::abi_views::plsm_view_time(owner) },
                timestamp
            );
            unsafe {
                crate::abi_views::plsm_view_release(owner);
            }
        }
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

    #[test]
    fn numeric_receipts_preserve_output_and_sequence_on_rejected_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut owner = plsm_pool {
            pool: Pool::create(dir.path().join("identities"), PoolOptions::new(64 * 1024))
                .unwrap()
                .into(),
        };
        let pool = &mut owner as *mut plsm_pool;
        let mut error = ptr::null_mut();
        let mut identity = plsm_feed_receipt {
            seq: 999,
            time: 888,
        };
        let json = br#"{"x":1}"#;
        let options = |durability| plsm_feed_options {
            struct_size: std::mem::size_of::<plsm_feed_options>() as u32,
            durability,
            tags: ptr::null(),
            tags_len: 0,
        };
        assert_eq!(
            plsm_pool_feed_json_receipt(
                pool,
                json.as_ptr(),
                json.len(),
                &options(0),
                ptr::null_mut(),
                &mut error
            ),
            -1
        );
        plsm_error_free(error);
        error = ptr::null_mut();
        for (input, durability) in [(b"bad".as_slice(), 0), (json.as_slice(), 42)] {
            assert_ne!(
                plsm_pool_feed_json_receipt(
                    pool,
                    input.as_ptr(),
                    input.len(),
                    &options(durability),
                    &mut identity,
                    &mut error
                ),
                0
            );
            assert_eq!((identity.seq, identity.time), (999, 888));
            plsm_error_free(error);
            error = ptr::null_mut();
        }
        assert_ne!(
            plsm_pool_feed_lite3_receipt(
                pool,
                json.as_ptr(),
                json.len(),
                ptr::null(),
                &mut identity,
                &mut error
            ),
            0
        );
        assert_eq!((identity.seq, identity.time), (999, 888));
        plsm_error_free(error);
        error = ptr::null_mut();
        assert_eq!(
            plsm_pool_feed_json_receipt(
                pool,
                json.as_ptr(),
                json.len(),
                &options(1),
                &mut identity,
                &mut error
            ),
            0
        );
        assert_eq!(identity.seq, 1);
        let PoolHandle::Local(storage) = &owner.pool else {
            unreachable!()
        };
        let frame = storage.get(1).unwrap();
        assert_eq!(identity.time, frame.time);
        assert!(identity.time > (1_u64 << 53));
        assert_eq!(
            plsm_pool_feed_lite3_receipt(
                pool,
                frame.payload.as_ptr(),
                frame.payload.len(),
                ptr::null(),
                &mut identity,
                &mut error
            ),
            0
        );
        assert_eq!(identity.seq, 2);
        let PoolHandle::Local(storage) = &owner.pool else {
            unreachable!()
        };
        assert_eq!(identity.time, storage.get(2).unwrap().time);
    }

    #[test]
    fn abi_json_feed_returns_stored_value_and_accepts_unaligned_lite3() {
        let dir = tempfile::tempdir().unwrap();
        let mut owner = plsm_pool {
            pool: Pool::create(dir.path().join("feed"), PoolOptions::new(65536))
                .unwrap()
                .into(),
        };
        let pool = &mut owner as *mut plsm_pool;
        let mut error = ptr::null_mut();
        let mut out = plsm_buf {
            data: ptr::null_mut(),
            len: 0,
        };
        let json = br#"{"n":18446744073709551615}"#;
        assert_eq!(
            plsm_pool_feed_json(
                pool,
                json.as_ptr(),
                json.len(),
                ptr::null(),
                &mut out,
                &mut error
            ),
            0
        );
        let receipt = parse_buf(&out);
        plsm_buf_free(&mut out);

        let PoolHandle::Local(storage) = &owner.pool else {
            unreachable!()
        };
        let frame = storage.get(1).unwrap();
        let stored: serde_json::Value =
            serde_json::from_slice(&crate::api::message_json_from_frame(&frame).unwrap()).unwrap();
        assert_eq!(receipt, stored);

        let mut prefixed = vec![0u32; (frame.payload.len() + 1).div_ceil(4)];
        let unaligned = unsafe { prefixed.as_mut_ptr().cast::<u8>().add(1) };
        unsafe {
            ptr::copy_nonoverlapping(frame.payload.as_ptr(), unaligned, frame.payload.len());
        }
        assert_eq!(
            plsm_pool_feed_lite3(
                pool,
                unaligned,
                frame.payload.len(),
                ptr::null(),
                &mut out,
                &mut error
            ),
            0
        );
        assert_eq!(parse_buf(&out)["data"], stored["data"]);
        plsm_buf_free(&mut out);

        let mut identity = plsm_feed_receipt { seq: 0, time: 0 };
        assert_eq!(
            plsm_pool_feed_lite3_receipt(
                pool,
                unaligned,
                frame.payload.len(),
                ptr::null(),
                &mut identity,
                &mut error
            ),
            0
        );
        assert_eq!(identity.seq, 3);
        assert!(error.is_null());
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
        let tags = [batches::ViewStreamText {
            data: b"tag".as_ptr(),
            len: 3,
        }];
        let mut out = plsm_buf {
            data: std::ptr::null_mut(),
            len: 0,
        };
        let rc = plsm_pool_feed_json(
            pool,
            payload.as_bytes().as_ptr(),
            payload.as_bytes().len(),
            &plsm_feed_options {
                struct_size: std::mem::size_of::<plsm_feed_options>() as u32,
                durability: 0,
                tags: tags.as_ptr(),
                tags_len: tags.len(),
            },
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
        let mut stream = ptr::null_mut();
        assert_eq!(plsm_pool_fetch(pool, ptr::null(), &mut stream, &mut err), 0);
        assert_eq!(plsm_stream_next_wait(stream, 0, &mut out, &mut err), 1);
        plsm_stream_free(stream);
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
        let mut receipt = plsm_feed_receipt { seq: 0, time: 0 };
        let rc = plsm_pool_feed_lite3_receipt(
            pool,
            payload.as_slice().as_ptr(),
            payload.len(),
            ptr::null(),
            &mut receipt,
            &mut err,
        );
        assert_eq!(rc, 0, "append lite3 failed");
        assert_eq!(receipt.seq, 1);

        let mut stream: *mut plsm_lite3_stream = std::ptr::null_mut();
        let rc = plsm_pool_fetch_lite3(pool, ptr::null(), &mut stream, &mut err);
        assert_eq!(rc, 0, "lite3 stream open failed");
        assert!(!stream.is_null());

        let mut frame = plsm_lite3_frame {
            seq: 0,
            time: 0,
            flags: 0,
            payload: plsm_buf {
                data: std::ptr::null_mut(),
                len: 0,
            },
        };
        let rc = plsm_lite3_stream_next_wait(stream, 0, &mut frame, &mut err);
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
    fn abi_pool_reference_path_matrix() {
        let mut err = std::ptr::null_mut();
        for raw in [
            "./bare.plasmite",
            "./space 東京.plasmite",
            "/tmp/pool.plasmite",
        ] {
            let input = CString::new(raw).unwrap();
            assert_eq!(
                parse_pool_ref(input.as_ptr(), &mut err).unwrap(),
                PoolRef::path(raw)
            );
        }
        let raw = "bare.plasmite";
        let input = CString::new(raw).unwrap();
        assert_eq!(
            parse_pool_ref(input.as_ptr(), &mut err).unwrap(),
            PoolRef::name(raw)
        );
        #[cfg(unix)]
        {
            let input = CString::new(r"literal\name").unwrap();
            assert_eq!(
                parse_pool_ref(input.as_ptr(), &mut err).unwrap(),
                PoolRef::name(r"literal\name")
            );
        }
        #[cfg(windows)]
        {
            for raw in [
                r"C:\data\space 東京.plasmite",
                r"\\server\share\pool.plasmite",
                r".\bare.plasmite",
                r"relative\pool.plasmite",
            ] {
                let input = CString::new(raw).unwrap();
                assert_eq!(
                    parse_pool_ref(input.as_ptr(), &mut err).unwrap(),
                    PoolRef::path(raw)
                );
            }
            let input = CString::new(r"C:relative\pool.plasmite").unwrap();
            assert!(parse_pool_ref(input.as_ptr(), &mut err).is_err());
            take_error(err);
        }
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
        let rc = plsm_pool_open(client, std::ptr::null(), 0, &mut pool, &mut err);
        assert_eq!(rc, -1);
        let (kind, message, _path, _seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::Usage));
        assert_eq!(message, "pool_ref is null");

        let remote = CString::new("http://example.com/pool").expect("cstr");
        let rc = plsm_pool_open(client, remote.as_ptr(), 0, &mut pool, &mut err);
        assert_eq!(rc, -1);
        let (kind, message, _path, _seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::Usage));
        assert_eq!(message, "remote HTTP is allowed only for the local machine");

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
        let rc = plsm_pool_open(client, pool_name.as_ptr(), 0, &mut pool, &mut err);
        assert_eq!(rc, -1);
        let (kind, _message, path, _seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::NotFound));
        let path = path.expect("path");
        assert!(path.ends_with("missing.plasmite"));

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
        let rc = plsm_pool_feed_json(
            pool,
            std::ptr::null(),
            0,
            std::ptr::null(),
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

        let mut receipt = plsm_feed_receipt { seq: 0, time: 0 };
        let rc = plsm_pool_feed_lite3_receipt(
            pool,
            std::ptr::null(),
            0,
            ptr::null(),
            &mut receipt,
            &mut err,
        );
        assert_eq!(rc, -1);
        let (kind, message, _path, _seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::Usage));
        assert_eq!(message, "lite3_bytes is null");

        let payload = [0u8; 8];
        let rc = plsm_pool_feed_lite3_receipt(
            pool,
            payload.as_ptr(),
            payload.len(),
            ptr::null(),
            &mut receipt,
            &mut err,
        );
        assert_eq!(rc, -1);
        let (kind, _message, _path, _seq, _offset) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::Corrupt));

        let scalar =
            crate::core::lite3::Lite3Buf::from_json_str(r#"{"meta":{"tags":[]},"data":1}"#)
                .unwrap();
        let bytes = scalar.as_slice();
        assert_eq!(
            plsm_pool_feed_lite3_receipt(
                pool,
                bytes.as_ptr(),
                bytes.len(),
                ptr::null(),
                &mut receipt,
                &mut err,
            ),
            -1
        );
        let (kind, ..) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::Usage));

        // An aligned frame can still contain an unaligned internal child.
        let valid = crate::core::lite3::encode_message(&[], &serde_json::json!({"x": true}))
            .expect("encode");
        let data = valid.as_doc().key_offset("data").expect("data");
        let mut malformed = valid.as_slice().to_vec();
        malformed[data + 64..data + 68].copy_from_slice(&1u32.to_le_bytes());
        let rc = plsm_pool_feed_lite3_receipt(
            pool,
            malformed.as_ptr(),
            malformed.len(),
            ptr::null(),
            &mut receipt,
            &mut err,
        );
        assert_eq!(rc, -1);
        assert_eq!(
            receipt.seq, 0,
            "rejection leaves the sequence output unchanged"
        );
        let (kind, ..) = take_error(err);
        assert_eq!(kind, error_kind_code(ErrorKind::Corrupt));
        let rc = plsm_pool_feed_lite3_receipt(
            pool,
            valid.as_slice().as_ptr(),
            valid.len(),
            ptr::null(),
            &mut receipt,
            &mut err,
        );
        assert_eq!(rc, 0, "valid append after rejection");
        assert_eq!(receipt.seq, 1, "rejection consumes no sequence");

        plsm_pool_free(pool);
        plsm_client_free(client);
    }
}

#[path = "abi_batches.rs"]
mod batches;
