//! C ownership and borrowed access for immutable Lite3 snapshots.
#![allow(clippy::result_large_err)]
use crate::abi::{borrow_pool, fail, plsm_buf, plsm_error, plsm_pool, write_json_buf};
use crate::core::error::{Error, ErrorKind};
use crate::view::{MessageView, ValueRef};
use std::sync::Arc;

#[allow(non_camel_case_types)]
pub struct plsm_message_view {
    view: MessageView,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct plsm_value_ref {
    owner: *const plsm_message_view,
    offset: usize,
}
#[repr(C)]
#[derive(Default)]
pub struct plsm_view_value {
    kind: u32,
    integer: i64,
    real: f64,
    bytes: *const u8,
    len: usize,
}
#[repr(C)]
pub struct plsm_view_entry {
    key: *const u8,
    key_len: usize,
    value: plsm_value_ref,
}
fn usage() -> Error {
    Error::new(ErrorKind::Usage).with_message("invalid view argument")
}
fn finish(r: Result<(), Error>, err: *mut *mut plsm_error) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => fail(err, e),
    }
}
pub(crate) unsafe fn owner_view<'a>(owner: *const plsm_message_view) -> Option<&'a MessageView> {
    unsafe { owner.as_ref() }.map(|owner| &owner.view)
}

pub(crate) unsafe fn value<'a>(v: plsm_value_ref) -> Result<ValueRef<'a>, Error> {
    let owner = unsafe { v.owner.as_ref() }.ok_or_else(usage)?;
    // The C contract requires refs obtained from this live owner, unchanged.
    Ok(unsafe { owner.view.value_at(v.offset) })
}
fn reference(owner: *const plsm_message_view, v: ValueRef<'_>) -> plsm_value_ref {
    plsm_value_ref {
        owner,
        offset: v.offset(),
    }
}
pub(crate) fn publish(view: MessageView, out: *mut *const plsm_message_view) -> Result<(), Error> {
    if out.is_null() {
        return Err(usage());
    }
    unsafe {
        *out = Arc::into_raw(Arc::new(plsm_message_view { view }));
    }
    Ok(())
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_view_from_lite3(
    bytes: *const u8,
    len: usize,
    seq: u64,
    timestamp_ns: u64,
    out: *mut *const plsm_message_view,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            if bytes.is_null() {
                return Err(usage());
            }
            publish(
                MessageView::from_bytes(
                    unsafe { std::slice::from_raw_parts(bytes, len) },
                    seq,
                    timestamp_ns,
                )?,
                out,
            )
        })(),
        err,
    )
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_get_view(
    pool: *mut plsm_pool,
    seq: u64,
    out: *mut *const plsm_message_view,
    err: *mut *mut plsm_error,
) -> i32 {
    let pool = match borrow_pool(pool, err) {
        Ok(p) => p,
        Err(e) => return e,
    };
    finish(
        (|| {
            let f = pool.pool.get(seq)?;
            publish(
                MessageView::from_owned(f.payload, f.seq, f.timestamp_ns, f.flags)?,
                out,
            )
        })(),
        err,
    )
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_view_retain(owner: *const plsm_message_view) {
    if !owner.is_null() {
        unsafe {
            Arc::increment_strong_count(owner);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_view_release(owner: *const plsm_message_view) {
    if !owner.is_null() {
        unsafe {
            Arc::decrement_strong_count(owner);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_view_data(owner: *const plsm_message_view) -> plsm_value_ref {
    match unsafe { owner.as_ref() } {
        Some(o) => reference(owner, o.view.data()),
        None => plsm_value_ref::default(),
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_view_seq(owner: *const plsm_message_view) -> u64 {
    unsafe { (*owner).view.seq }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_view_timestamp_ns(owner: *const plsm_message_view) -> u64 {
    unsafe { (*owner).view.timestamp_ns }
}
/// Backing payload capacity; no owner/index metadata or double-counted shares.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_view_retained_bytes(owner: *const plsm_message_view) -> usize {
    unsafe { owner.as_ref() }.map_or(0, |owner| owner.view.retained_bytes())
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_value_get(
    v: plsm_value_ref,
    key: *const u8,
    len: usize,
    out: *mut plsm_value_ref,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            if out.is_null() || key.is_null() {
                return Err(usage());
            }
            let key = std::str::from_utf8(unsafe { std::slice::from_raw_parts(key, len) })
                .map_err(|_| usage())?;
            let node = unsafe { value(v) }?.get(key)?;
            unsafe {
                *out = node.map(|n| reference(v.owner, n)).unwrap_or_default();
            }
            Ok(())
        })(),
        err,
    )
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_value_index(
    v: plsm_value_ref,
    index: u32,
    out: *mut plsm_value_ref,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            if out.is_null() {
                return Err(usage());
            }
            let node = unsafe { value(v) }?.index(index)?;
            unsafe {
                *out = node.map(|n| reference(v.owner, n)).unwrap_or_default();
            }
            Ok(())
        })(),
        err,
    )
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_value_read(
    v: plsm_value_ref,
    out: *mut plsm_view_value,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            if out.is_null() {
                return Err(usage());
            }
            if v.owner.is_null() {
                unsafe {
                    *out = plsm_view_value::default();
                }
                return Ok(());
            }
            let node = unsafe { value(v) }?;
            let span = node.span();
            let len = match span {
                Some(s) => s.len(),
                None => {
                    if matches!(node.kind(), 6 | 7) {
                        node.len()?
                    } else {
                        0
                    }
                }
            };
            unsafe {
                *out = plsm_view_value {
                    kind: match node.kind() {
                        4 => 6,
                        5 => 5,
                        kind => u32::from(kind) + 1,
                    },
                    integer: node.integer().unwrap_or_default(),
                    real: node.real().unwrap_or_default(),
                    bytes: span.map_or(std::ptr::null(), |s| s.as_ptr()),
                    len,
                };
            }
            Ok(())
        })(),
        err,
    )
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_value_entries(
    v: plsm_value_ref,
    slots: *mut plsm_view_entry,
    capacity: usize,
    out_count: *mut usize,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            if out_count.is_null() {
                return Err(usage());
            }
            let node = unsafe { value(v) }?;
            let count = node.len()?;
            unsafe {
                *out_count = count;
            }
            if capacity < count {
                return Err(Error::new(ErrorKind::Usage).with_message("entry buffer too small"));
            }
            if count > 0 && slots.is_null() {
                return Err(usage());
            }
            for (i, (key, child)) in node.entries()?.into_iter().enumerate() {
                unsafe {
                    *slots.add(i) = plsm_view_entry {
                        key: key.map_or(std::ptr::null(), |s| s.as_ptr()),
                        key_len: key.map_or(0, str::len),
                        value: reference(v.owner, child),
                    };
                }
            }
            Ok(())
        })(),
        err,
    )
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_value_to_json(
    v: plsm_value_ref,
    out: *mut plsm_buf,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| write_json_buf(out, unsafe { value(v) }?.to_json()?))(),
        err,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_view_root(owner: *const plsm_message_view) -> plsm_value_ref {
    match unsafe { owner.as_ref() } {
        Some(o) => reference(owner, o.view.root()),
        None => plsm_value_ref::default(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_view_json_native_safe(owner: *const plsm_message_view) -> bool {
    unsafe { owner.as_ref() }.is_some_and(|owner| owner.view.json_native_safe())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_view_bytes(
    owner: *const plsm_message_view,
    len: *mut usize,
) -> *const u8 {
    if owner.is_null() || len.is_null() {
        return std::ptr::null();
    }
    let bytes = unsafe { (*owner).view.bytes() };
    unsafe {
        *len = bytes.len();
    }
    bytes.as_ptr()
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_get_into(
    pool: *mut plsm_pool,
    seq: u64,
    destination: *mut u8,
    capacity: usize,
    required: *mut usize,
    timestamp_ns: *mut u64,
    flags: *mut u32,
    err: *mut *mut plsm_error,
) -> i32 {
    let pool = match borrow_pool(pool, err) {
        Ok(p) => p,
        Err(e) => return e,
    };
    finish(
        (|| {
            if required.is_null()
                || timestamp_ns.is_null()
                || flags.is_null()
                || (capacity > 0 && destination.is_null())
            {
                return Err(usage());
            }
            if capacity > 0 && destination.align_offset(4) != 0 {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("destination must be four-byte aligned"));
            }
            let bytes = if capacity == 0 {
                &mut []
            } else {
                unsafe { std::slice::from_raw_parts_mut(destination, capacity) }
            };
            let (len, time, frame_flags) = pool.pool.get_into(seq, bytes)?;
            unsafe {
                *required = len;
                *timestamp_ns = time;
                *flags = frame_flags;
            }
            Ok(())
        })(),
        err,
    )
}

/// Paths parsed once for repeated coarse selection from immutable snapshots.
pub struct PlsmSelector {
    pub(crate) paths: Vec<Vec<String>>,
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_selector_new(
    paths: *const *const std::os::raw::c_char,
    count: usize,
    out: *mut *mut PlsmSelector,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            if out.is_null() {
                return Err(usage());
            }
            let paths = unsafe { crate::abi_fields::paths(paths, count) }?;
            let paths = paths
                .into_iter()
                .map(crate::abi_fields::pointer_tokens)
                .collect::<Result<Vec<_>, _>>()?;
            unsafe {
                *out = Box::into_raw(Box::new(PlsmSelector { paths }));
            }
            Ok(())
        })(),
        err,
    )
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_selector_free(selector: *mut PlsmSelector) {
    if !selector.is_null() {
        drop(unsafe { Box::from_raw(selector) });
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_view_select(
    owner: *const plsm_message_view,
    selector: *const PlsmSelector,
    slots: *mut plsm_value_ref,
    capacity: usize,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            let owner_ref = unsafe { owner.as_ref() }.ok_or_else(usage)?;
            let selector = unsafe { selector.as_ref() }.ok_or_else(usage)?;
            if capacity < selector.paths.len() || (!selector.paths.is_empty() && slots.is_null()) {
                return Err(usage());
            }
            for (i, path) in selector.paths.iter().enumerate() {
                let mut node = Some(owner_ref.view.data());
                for key in path {
                    node = match node {
                        None => None,
                        Some(v) => match v.kind() {
                            6 => v.get(key)?,
                            7 => {
                                if key.is_empty()
                                    || (key.len() > 1 && key.starts_with('0'))
                                    || !key.bytes().all(|b| b.is_ascii_digit())
                                {
                                    return Err(usage());
                                }
                                match key.parse::<u32>() {
                                    Ok(index) => v.index(index)?,
                                    Err(_) => None,
                                }
                            }
                            _ => None,
                        },
                    };
                }
                unsafe {
                    *slots.add(i) = node.map(|v| reference(owner, v)).unwrap_or_default();
                }
            }
            Ok(())
        })(),
        err,
    )
}

/// Parse once, commit the checked encoding, and retain it as an owned view.
#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_append_view_json(
    pool: *mut plsm_pool,
    bytes: *const u8,
    len: usize,
    tags: *const *const std::ffi::c_char,
    tags_len: usize,
    durability: u32,
    out: *mut *const plsm_message_view,
    err: *mut *mut plsm_error,
) -> i32 {
    if bytes.is_null() || out.is_null() {
        return fail(err, usage());
    }
    let pool = match borrow_pool(pool, err) {
        Ok(p) => p,
        Err(e) => return e,
    };
    finish(
        (|| {
            let durability = match durability {
                0 => crate::api::Durability::Fast,
                1 => crate::api::Durability::Flush,
                _ => return Err(usage()),
            };
            let parsed = crate::core::lite3::ParsedJson::parse_view(unsafe {
                std::slice::from_raw_parts(bytes, len)
            })?;
            let tags = unsafe { crate::abi::tag_refs(tags, tags_len) }?;
            let mut view = parsed.encode_message_view(&tags)?;
            let time = crate::api::now_ns()?;
            let seq = pool.pool.append_with_options(
                view.bytes(),
                crate::api::AppendOptions::new(time, durability),
            )?;
            view.seq = seq;
            view.timestamp_ns = time;
            publish(view, out)
        })(),
        err,
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_value_to_json_stored(
    v: plsm_value_ref,
    out: *mut plsm_buf,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| write_json_buf(out, unsafe { value(v) }?.to_json_stored()?))(),
        err,
    )
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_view_to_json(
    owner: *const plsm_message_view,
    out: *mut plsm_buf,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            let owner = unsafe { owner.as_ref() }.ok_or_else(usage)?;
            write_json_buf(out, owner.view.to_json()?)
        })(),
        err,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lite3::Lite3Buf;
    #[test]
    fn retained_owner_and_prepared_selector_keep_borrowed_results() {
        assert!(!unsafe { plsm_view_json_native_safe(std::ptr::null()) });
        let mut bytes = Lite3Buf::from_json_str(
            r#"{"meta":{"tags":[]},"data":{"nested":{"x":7},"s":"hello"}}"#,
        )
        .unwrap()
        .as_slice()
        .to_vec();
        let mut owner = std::ptr::null();
        let mut err = std::ptr::null_mut();
        assert_eq!(
            plsm_view_from_lite3(bytes.as_ptr(), bytes.len(), 17, 99, &mut owner, &mut err),
            0
        );
        assert!(unsafe { plsm_view_json_native_safe(owner) });
        bytes.fill(0);
        unsafe {
            plsm_view_retain(owner);
            plsm_view_release(owner);
        }
        let paths = [c"/nested/x".as_ptr(), c"/s".as_ptr(), c"/missing".as_ptr()];
        let mut selector = std::ptr::null_mut();
        assert_eq!(
            plsm_selector_new(paths.as_ptr(), 3, &mut selector, &mut err),
            0
        );
        let mut slots = [plsm_value_ref::default(); 3];
        for _ in 0..3 {
            assert_eq!(
                plsm_view_select(owner, selector, slots.as_mut_ptr(), 3, &mut err),
                0
            );
            let mut out = plsm_view_value::default();
            assert_eq!(plsm_value_read(slots[0], &mut out, &mut err), 0);
            assert_eq!((out.kind, out.integer), (3, 7));
            assert_eq!(plsm_value_read(slots[1], &mut out, &mut err), 0);
            assert_eq!(out.kind, 5);
            assert_eq!(
                unsafe { std::slice::from_raw_parts(out.bytes, out.len) },
                b"hello"
            );
            assert_eq!(plsm_value_read(slots[2], &mut out, &mut err), 0);
            assert_eq!(out.kind, 0);
        }
        assert_eq!(unsafe { plsm_view_seq(owner) }, 17);
        unsafe {
            plsm_selector_free(selector);
            plsm_view_release(owner);
        }
    }
}

/// Forward immutable bytes with a fresh destination identity and no payload copy.
#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_append_view(
    pool: *mut plsm_pool,
    source: *const plsm_message_view,
    durability: u32,
    out: *mut *const plsm_message_view,
    err: *mut *mut plsm_error,
) -> i32 {
    if source.is_null() || out.is_null() {
        return fail(err, usage());
    }
    let pool = match borrow_pool(pool, err) {
        Ok(p) => p,
        Err(e) => return e,
    };
    finish(
        (|| {
            let source = unsafe { &*source };
            let durability = match durability {
                0 => crate::api::Durability::Fast,
                1 => crate::api::Durability::Flush,
                _ => return Err(usage()),
            };
            let time = crate::api::now_ns()?;
            let seq = pool.pool.append_with_options(
                source.view.bytes(),
                crate::api::AppendOptions::new(time, durability),
            )?;
            publish(source.view.with_identity(seq, time, source.view.flags), out)
        })(),
        err,
    )
}

#[cfg(test)]
mod forwarding_tests {
    use super::*;
    #[test]
    fn forwarding_shares_payload_with_fresh_identity_after_pool_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = std::ffi::CString::new(dir.path().to_str().unwrap()).unwrap();
        let mut client = std::ptr::null_mut();
        let mut pool = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        assert_eq!(
            crate::abi::plsm_client_new(path.as_ptr(), &mut client, &mut err),
            0
        );
        assert_eq!(
            crate::abi::plsm_pool_create(
                client,
                c"test".as_ptr(),
                1024 * 1024,
                &mut pool,
                &mut err
            ),
            0
        );
        let bytes =
            crate::core::lite3::Lite3Buf::from_json_str(r#"{"meta":{"tags":[]},"data":{"x":7}}"#)
                .unwrap();
        let mut source = std::ptr::null();
        let mut forwarded = std::ptr::null();
        assert_eq!(
            plsm_view_from_lite3(
                bytes.as_slice().as_ptr(),
                bytes.len(),
                99,
                123,
                &mut source,
                &mut err
            ),
            0
        );
        assert_eq!(
            plsm_pool_append_view(pool, source, 0, &mut forwarded, &mut err),
            0
        );
        let mut old_len = 0;
        let mut new_len = 0;
        assert_eq!(unsafe { plsm_view_bytes(source, &mut old_len) }, unsafe {
            plsm_view_bytes(forwarded, &mut new_len)
        });
        assert_eq!(old_len, new_len);
        assert_eq!(unsafe { plsm_view_seq(forwarded) }, 1);
        assert_ne!(unsafe { plsm_view_timestamp_ns(forwarded) }, 123);
        crate::abi::plsm_pool_free(pool);
        crate::abi::plsm_client_free(client);
        unsafe {
            plsm_view_release(source);
        }
        let data = unsafe { plsm_view_data(forwarded) };
        let mut selected = plsm_value_ref::default();
        assert_eq!(
            plsm_value_get(data, b"x".as_ptr(), 1, &mut selected, &mut err),
            0
        );
        let mut out = plsm_view_value::default();
        assert_eq!(plsm_value_read(selected, &mut out, &mut err), 0);
        assert_eq!(out.integer, 7);
        unsafe {
            plsm_view_release(forwarded);
        }
    }
}

/// One bounded iterator state and one owner reference; no entry array is retained.
pub struct PlsmViewCursor {
    owner: Arc<plsm_message_view>,
    cursor: crate::view::NodeCursor,
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_value_iter(
    v: plsm_value_ref,
    out: *mut *mut PlsmViewCursor,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            if out.is_null() {
                return Err(usage());
            }
            let node = unsafe { value(v) }?;
            let cursor = crate::view::NodeCursor::new(unsafe { &(*v.owner).view }, node.offset())?;
            unsafe {
                Arc::increment_strong_count(v.owner);
                *out = Box::into_raw(Box::new(PlsmViewCursor {
                    owner: Arc::from_raw(v.owner),
                    cursor,
                }));
            }
            Ok(())
        })(),
        err,
    )
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_cursor_next(
    cursor: *mut PlsmViewCursor,
    slots: *mut plsm_view_entry,
    capacity: usize,
    written: *mut usize,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            let cursor = unsafe { cursor.as_mut() }.ok_or_else(usage)?;
            if written.is_null() || (capacity > 0 && slots.is_null()) {
                return Err(usage());
            }
            unsafe {
                *written = 0;
            }
            for i in 0..capacity {
                let Some((key, offset, _)) = cursor.cursor.next(&cursor.owner.view)? else {
                    break;
                };
                unsafe {
                    *slots.add(i) = plsm_view_entry {
                        key: key.map_or(std::ptr::null(), |s| s.as_ptr()),
                        key_len: key.map_or(0, str::len),
                        value: plsm_value_ref {
                            owner: Arc::as_ptr(&cursor.owner),
                            offset,
                        },
                    };
                    *written = i + 1;
                }
            }
            Ok(())
        })(),
        err,
    )
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_cursor_free(cursor: *mut PlsmViewCursor) {
    if !cursor.is_null() {
        drop(unsafe { Box::from_raw(cursor) });
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;
    #[test]
    fn cursor_reads_bounded_chunks_and_retains_its_owner() {
        let samples: Vec<_> = (0..1000).collect();
        let encoded = crate::core::lite3::Lite3Buf::from_json_str(
            &serde_json::json!({"meta":{"tags":[]},"data":{"samples":samples}}).to_string(),
        )
        .unwrap();
        let mut owner = std::ptr::null();
        let mut err = std::ptr::null_mut();
        assert_eq!(
            plsm_view_from_lite3(
                encoded.as_slice().as_ptr(),
                encoded.len(),
                1,
                2,
                &mut owner,
                &mut err
            ),
            0
        );
        let mut array = plsm_value_ref::default();
        assert_eq!(
            plsm_value_get(
                unsafe { plsm_view_data(owner) },
                b"samples".as_ptr(),
                7,
                &mut array,
                &mut err
            ),
            0
        );
        let mut cursor = std::ptr::null_mut();
        assert_eq!(plsm_value_iter(array, &mut cursor, &mut err), 0);
        unsafe {
            plsm_view_release(owner);
        }
        let mut slots: [plsm_view_entry; 7] = std::array::from_fn(|_| plsm_view_entry {
            key: std::ptr::null(),
            key_len: 0,
            value: plsm_value_ref::default(),
        });
        let mut sum = 0;
        let mut count = 0;
        loop {
            let mut written = 0;
            assert_eq!(
                plsm_cursor_next(cursor, slots.as_mut_ptr(), 7, &mut written, &mut err),
                0
            );
            assert!(written <= 7);
            if written == 0 {
                break;
            }
            for slot in &slots[..written] {
                assert!(slot.key.is_null());
                let mut scalar = plsm_view_value::default();
                assert_eq!(plsm_value_read(slot.value, &mut scalar, &mut err), 0);
                sum += scalar.integer;
                count += 1;
            }
        }
        assert_eq!(count, 1000);
        assert_eq!(sum, 499500);
        unsafe {
            plsm_cursor_free(cursor);
        }
    }
}
