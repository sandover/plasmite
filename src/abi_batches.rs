//! Explicit bounded acquisition of independent message owners.
use super::*;
use crate::abi_views::{plsm_message_view, publish};
use crate::view::MessageView;

#[repr(C)]
pub struct ViewStreamText {
    pub(super) data: *const u8,
    pub(super) len: usize,
}

// The caller keeps the array and each byte span valid for the whole call.
pub(super) unsafe fn tag_spans<'a>(
    tags: *const ViewStreamText,
    count: usize,
) -> Result<Vec<&'a str>, Error> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if tags.is_null() {
        return Err(Error::new(ErrorKind::Usage).with_message("null tag array"));
    }
    unsafe { std::slice::from_raw_parts(tags, count) }
        .iter()
        .map(|span| {
            let bytes = if span.len == 0 {
                &[][..]
            } else {
                unsafe { feed_input_bytes(span.data, span.len, "tag bytes") }?
            };
            std::str::from_utf8(bytes)
                .map_err(|_| Error::new(ErrorKind::Usage).with_message("tag is not UTF-8"))
        })
        .collect()
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
            MessageView::from_owned(frame.payload, frame.seq, frame.time, frame.flags).map(Some)
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
