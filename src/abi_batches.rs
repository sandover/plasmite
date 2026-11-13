//! Explicit bounded acquisition of independent message owners.
use super::*;
use crate::abi_views::{plsm_message_view, publish};
use crate::view::MessageView;

#[repr(C)]
pub struct ViewStreamText {
    data: *const u8,
    len: usize,
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_view_stream_open(
    pool: *mut plsm_pool,
    options: *const plsm_stream_options,
    tags: *const ViewStreamText,
    tag_count: usize,
    out: *mut *mut plsm_stream,
    error: *mut *mut plsm_error,
) -> i32 {
    let options = match parse_stream_options(options, error) {
        Ok(options) => options,
        Err(code) => return code,
    };
    let pool = match borrow_pool(pool, error) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let result = (|| {
        if out.is_null() || (tag_count > 0 && tags.is_null()) {
            return Err(Error::new(ErrorKind::Usage).with_message("null view stream argument"));
        }
        let spans = if tag_count == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(tags, tag_count) }
        };
        let tags = spans
            .iter()
            .map(|span| {
                if span.len > 0 && span.data.is_null() {
                    return Err(Error::new(ErrorKind::Usage).with_message("null tag span"));
                }
                let bytes = if span.len == 0 {
                    &[][..]
                } else {
                    unsafe { std::slice::from_raw_parts(span.data, span.len) }
                };
                std::str::from_utf8(bytes)
                    .map(str::to_owned)
                    .map_err(|_| Error::new(ErrorKind::Usage).with_message("tag is not UTF-8"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let state = TailState::new(
            &pool.pool,
            TailOptions {
                since_seq: options.since_seq,
                max_messages: options.max_messages,
                timeout: options.timeout,
                gap_policy: options.gap_policy,
                tags,
                notify: true,
                cancel: Some(cancel.clone()),
                ..TailOptions::default()
            },
        );
        let info = pool.pool.info()?;
        let pool = Pool::open(&info.path)?;
        unsafe {
            *out = Box::into_raw(Box::new(plsm_stream {
                pool,
                state: Mutex::new(state),
                cancel,
            }));
        }
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(err) => fail(error, err),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_view_stream_next(
    stream: *mut plsm_stream,
    timeout_ms: u64,
    max_count: usize,
    max_bytes: usize,
    out: *mut *const plsm_message_view,
    count: *mut usize,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() || count.is_null() {
        return fail(
            error,
            Error::new(ErrorKind::Usage).with_message("output is null"),
        );
    }
    unsafe {
        *count = 0;
    }
    let stream = match borrow_stream(stream, error) {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    let result = (|| {
        let mut state = stream.state.lock().map_err(|_| {
            Error::new(ErrorKind::Internal).with_message("stream state is unavailable")
        })?;
        let wait = (timeout_ms != u64::MAX).then(|| Duration::from_millis(timeout_ms));
        let select = |frame: crate::api::FrameRef, tags: &[String]| {
            if !stream_frame_matches_tags(&frame, tags)? {
                return Ok(None);
            }
            MessageView::from_owned(frame.payload, frame.seq, frame.timestamp_ns, frame.flags)
                .map(Some)
        };
        // The ordinary iterator never allocates a companion/checkpoint collection.
        if max_count == 1 {
            return match state.next_wait_bounded(&stream.pool, wait, max_bytes, select)? {
                TailOutcome::Item(message) => {
                    publish(message, out)?;
                    unsafe {
                        *count = 1;
                    }
                    Ok(TailOutcome::Item(Vec::new()))
                }
                TailOutcome::WaitExpired => Ok(TailOutcome::WaitExpired),
                TailOutcome::Finished => Ok(TailOutcome::Finished),
                TailOutcome::Cancelled => Ok(TailOutcome::Cancelled),
            };
        }
        state.next_batch_wait(&stream.pool, wait, max_count, max_bytes, select)
    })();
    match result {
        Ok(TailOutcome::Item(messages)) => {
            for (index, message) in messages.into_iter().enumerate() {
                if let Err(err) = publish(message, unsafe { out.add(index) }) {
                    return fail(error, err);
                }
                unsafe {
                    *count = index + 1;
                }
            }
            1
        }
        Ok(TailOutcome::WaitExpired) => 0,
        Ok(TailOutcome::Finished) => 2,
        Ok(TailOutcome::Cancelled) => 3,
        Err(err) => fail(error, err),
    }
}
