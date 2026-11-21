//! Start remote operations and report results without blocking a binding thread.
use super::*;
use crate::abi_views::{plsm_message_view, publish};
use crate::api::{AsyncHttpRead, AsyncUdpRead, FeedInput, stream_runtime};
use crate::view::MessageView;
use std::ffi::c_void;
use std::future::Future;
use tokio::sync::{Mutex as AsyncMutex, oneshot};

type Report = extern "C" fn(u64, i32, *const c_void, *mut plsm_error);

pub struct Job {
    _stop: oneshot::Sender<()>,
}

pub struct NetworkRead(Arc<AsyncMutex<Reader>>);

impl NetworkRead {
    fn new(reader: Reader) -> Self {
        Self(Arc::new(AsyncMutex::new(reader)))
    }
}

enum Reader {
    Http(AsyncHttpRead),
    Udp(AsyncUdpRead),
}

enum Item {
    Reader(NetworkRead),
    Message(MessageView),
}

fn start(
    token: u64,
    report: Report,
    out: *mut *mut Job,
    error: *mut *mut plsm_error,
    run: impl Future<Output = Result<Option<Item>, Error>> + Send + 'static,
) -> i32 {
    if out.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("out_job is null"),
        );
    }
    let runtime = match stream_runtime() {
        Ok(runtime) => runtime,
        Err(err) => return fail(error, err),
    };
    let (stop, stopped) = oneshot::channel();
    runtime.spawn(async move {
        let result = tokio::select! {
            biased;
            _ = stopped => None,
            result = run => Some(result),
        };
        let Some(result) = result else {
            report(token, 3, ptr::null(), ptr::null_mut());
            return;
        };
        match result {
            Ok(Some(Item::Reader(reader))) => {
                let reader = Box::new(reader);
                report(token, 4, Box::into_raw(reader).cast(), ptr::null_mut());
            }
            Ok(Some(Item::Message(message))) => {
                let mut owner = ptr::null::<plsm_message_view>();
                match publish(message, &mut owner) {
                    Ok(()) => report(token, 1, owner.cast(), ptr::null_mut()),
                    Err(err) => report_error(token, report, err),
                }
            }
            Ok(None) => report(token, 2, ptr::null(), ptr::null_mut()),
            Err(err) => report_error(token, report, err),
        }
    });
    unsafe { *out = Box::into_raw(Box::new(Job { _stop: stop })) };
    0
}

fn report_error(token: u64, report: Report, err: Error) {
    let mut error = ptr::null_mut();
    fail(&mut error, err);
    report(token, -1, ptr::null(), error);
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_internal_async_job_free(job: *mut Job) {
    if !job.is_null() {
        unsafe { drop(Box::from_raw(job)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_internal_async_network_close(reader: *mut NetworkRead) {
    if !reader.is_null() {
        unsafe { drop(Box::from_raw(reader)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_internal_async_network_read(
    http: *const plsm_pool,
    udp: *const plsm_udp_pool,
    options: *const plsm_read_options,
    live: bool,
    token: u64,
    report: Report,
    out: *mut *mut Job,
    error: *mut *mut plsm_error,
) -> i32 {
    let read = match parse_read(options) {
        Ok(read) => read,
        Err(err) => return fail(error, err),
    };
    if let Some(udp) = unsafe { udp.as_ref() } {
        let pool = udp.pool.clone();
        return start(token, report, out, error, async move {
            AsyncUdpRead::open(&pool, read, live)
                .await
                .map(|reader| Some(Item::Reader(NetworkRead::new(Reader::Udp(reader)))))
        });
    }
    let Some(PoolHandle::Remote(pool)) = unsafe { http.as_ref() }.map(|pool| &pool.pool) else {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("expected a remote pool"),
        );
    };
    let pool = pool.clone();
    start(token, report, out, error, async move {
        AsyncHttpRead::open(&pool, read, live)
            .await
            .map(|reader| Some(Item::Reader(NetworkRead::new(Reader::Http(reader)))))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_internal_async_network_next(
    reader: *const NetworkRead,
    token: u64,
    report: Report,
    out: *mut *mut Job,
    error: *mut *mut plsm_error,
) -> i32 {
    let Some(reader) = (unsafe { reader.as_ref() }) else {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("reader is closed"),
        );
    };
    let reader = reader.0.clone();
    start(token, report, out, error, async move {
        let result = match &mut *reader.lock().await {
            Reader::Http(reader) => reader.next().await,
            Reader::Udp(reader) => reader.next().await,
        };
        result.map(|message| message.map(Item::Message))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_internal_async_http_feed(
    pool: *const plsm_pool,
    kind: u32,
    input: *const u8,
    len: usize,
    options: *const plsm_feed_options,
    token: u64,
    report: Report,
    out: *mut *mut Job,
    error: *mut *mut plsm_error,
) -> i32 {
    let Some(PoolHandle::Remote(pool)) = unsafe { pool.as_ref() }.map(|pool| &pool.pool) else {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("expected a remote pool"),
        );
    };
    let pool = pool.clone();
    let prepared = (|| {
        let bytes = unsafe { feed_input_bytes(input, len, "message") }?;
        let input = match kind {
            1 => FeedInput::Json(bytes),
            2 => FeedInput::Lite3(bytes),
            _ => return Err(Error::new(ErrorKind::Usage).with_message("invalid feed input kind")),
        };
        parse_feed(options)?.prepare(input)
    })();
    let (view, durability) = match prepared {
        Ok(prepared) => prepared,
        Err(err) => return fail(error, err),
    };
    start(token, report, out, error, async move {
        pool.feed_view_async(view, durability)
            .await
            .map(|message| Some(Item::Message(message)))
    })
}
