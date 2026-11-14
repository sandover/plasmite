//! Private Wasm adapter over the same checked snapshot and input builder.
#![allow(dead_code)] // The narrowly linked module imports shared core modules, not their host APIs.
extern crate self as libc;
pub use std::ffi::c_void;
// WASI SDK errno numbers used by the C shim; no WASI runtime imports exist.
pub const ENOBUFS: i32 = 42;
pub const ENOMEM: i32 = 48;
mod core;
pub use core::error::{Error, ErrorKind};
#[path = "../../../../src/abi_builder.rs"]
mod abi_builder;
#[allow(unused_imports, unused_macros)] // Shared host wire declarations include host-only macros.
#[path = "../../../../src/interface_wire.rs"]
mod interface_wire;
#[path = "../../../../src/view.rs"]
pub mod view;

mod api {
    use crate::{Error, ErrorKind};
    pub(crate) fn format_ts(ns: u64) -> Result<String, Error> {
        time::OffsetDateTime::from_unix_timestamp_nanos(ns as i128)
            .map_err(|err| Error::new(ErrorKind::Internal).with_source(err))?
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|err| Error::new(ErrorKind::Internal).with_source(err))
    }
}
mod abi {
    use crate::Error;
    #[allow(non_camel_case_types)]
    pub struct plsm_error(pub Error);
    pub fn fail(out: *mut *mut plsm_error, error: Error) -> i32 {
        if !out.is_null() {
            unsafe {
                *out = Box::into_raw(Box::new(plsm_error(error)));
            }
        }
        -1
    }
}
mod abi_views {
    use crate::{
        Error,
        view::{MessageView, ValueRef},
    };
    #[allow(non_camel_case_types)]
    pub struct plsm_message_view {
        pub view: MessageView,
    }
    #[allow(non_camel_case_types)]
    #[repr(C)]
    pub struct plsm_value_ref {
        pub owner: *const plsm_message_view,
        pub offset: usize,
    }
    pub unsafe fn value<'a>(reference: plsm_value_ref) -> Result<ValueRef<'a>, Error> {
        Ok(unsafe { (*reference.owner).view.value_at(reference.offset) })
    }
}

// C allocation stays in the same Rust-owned linear heap. Its small prefix keeps
// the exact layout for free/realloc; no caller-visible memory borrows escape.
use std::alloc::{Layout, alloc, dealloc};
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn malloc(size: usize) -> *mut u8 {
    let Some(total) = size.checked_add(16) else {
        return std::ptr::null_mut();
    };
    let Ok(layout) = Layout::from_size_align(total, 16) else {
        return std::ptr::null_mut();
    };
    let base = unsafe { alloc(layout) };
    if base.is_null() {
        return base;
    }
    unsafe {
        base.cast::<usize>().write(total);
        base.add(16)
    }
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn free(pointer: *mut u8) {
    if !pointer.is_null() {
        unsafe {
            let base = pointer.sub(16);
            dealloc(
                base,
                Layout::from_size_align_unchecked(base.cast::<usize>().read(), 16),
            );
        }
    }
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn realloc(pointer: *mut u8, size: usize) -> *mut u8 {
    if pointer.is_null() {
        return unsafe { malloc(size) };
    }
    if size == 0 {
        unsafe {
            free(pointer);
        }
        return std::ptr::null_mut();
    }
    let next = unsafe { malloc(size) };
    if !next.is_null() {
        unsafe {
            let previous = pointer.sub(16).cast::<usize>().read() - 16;
            std::ptr::copy_nonoverlapping(pointer, next, previous.min(size));
            free(pointer);
        }
    }
    next
}
#[unsafe(export_name = "errno")]
static mut ERRNO: i32 = 0;
#[unsafe(no_mangle)]
pub extern "C" fn __errno_location() -> *mut i32 {
    std::ptr::addr_of_mut!(ERRNO)
}
#[unsafe(no_mangle)]
pub extern "C" fn portable_version() -> u32 {
    1
}

use abi_builder::{Input, Text, plsm_builder};
use abi_views::{plsm_message_view as Owner, plsm_value_ref as Ref};
use core::lite3::sys;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
static ERROR: Mutex<(u32, Vec<u8>)> = Mutex::new((0, Vec::new()));
static OUTPUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_OWNERS: AtomicUsize = AtomicUsize::new(0);
fn failure(error: Error) {
    let kind = match error.kind() {
        ErrorKind::Usage => 2,
        ErrorKind::Corrupt => 7,
        _ => 1,
    };
    *ERROR.lock().unwrap() = (kind, error.to_string().into_bytes());
}
fn result<T>(value: Result<T, Error>, fallback: T) -> T {
    match value {
        Ok(value) => value,
        Err(error) => {
            failure(error);
            fallback
        }
    }
}
fn checked(code: i32, error: *mut abi::plsm_error) -> bool {
    if code == 0 {
        true
    } else {
        failure(unsafe { Box::from_raw(error) }.0);
        false
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn error_kind() -> u32 {
    ERROR.lock().unwrap().0
}
#[unsafe(no_mangle)]
pub extern "C" fn error_ptr() -> *const u8 {
    ERROR.lock().unwrap().1.as_ptr()
}
#[unsafe(no_mangle)]
pub extern "C" fn error_len() -> usize {
    ERROR.lock().unwrap().1.len()
}
#[unsafe(no_mangle)]
pub extern "C" fn output_ptr() -> *const u8 {
    OUTPUT.lock().unwrap().as_ptr()
}
#[unsafe(no_mangle)]
pub extern "C" fn output_len() -> usize {
    OUTPUT.lock().unwrap().len()
}
#[unsafe(no_mangle)]
pub extern "C" fn output_free() {
    *OUTPUT.lock().unwrap() = Vec::new();
}
#[unsafe(no_mangle)]
pub extern "C" fn live_bytes() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}
#[unsafe(no_mangle)]
pub extern "C" fn live_owners() -> usize {
    LIVE_OWNERS.load(Ordering::Relaxed)
}
#[unsafe(no_mangle)]
pub extern "C" fn input_alloc(len: usize) -> *mut u8 {
    if len == 0 || len > core::lite3::MAX_LITE3_BUF {
        return std::ptr::null_mut();
    }
    unsafe { alloc(Layout::array::<u8>(len).unwrap()) }
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn input_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        unsafe {
            dealloc(ptr, Layout::array::<u8>(len).unwrap());
        }
    }
}
fn own(view: view::MessageView) -> *mut Owner {
    LIVE_BYTES.fetch_add(view.retained_bytes(), Ordering::Relaxed);
    LIVE_OWNERS.fetch_add(1, Ordering::Relaxed);
    Box::into_raw(Box::new(Owner { view }))
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn owner_decode(ptr: *mut u8, len: usize) -> *mut Owner {
    // input_alloc and the JS adapter establish this allocation/layout and fill it.
    let bytes = if len == 0 {
        Vec::new()
    } else {
        unsafe { Vec::from_raw_parts(ptr, len, len) }
    };
    result(
        view::MessageView::from_owned(bytes, 0, 0, 0).map(own),
        std::ptr::null_mut(),
    )
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn owner_free(owner: *mut Owner) {
    let owner = unsafe { Box::from_raw(owner) };
    LIVE_BYTES.fetch_sub(owner.view.retained_bytes(), Ordering::Relaxed);
    LIVE_OWNERS.fetch_sub(1, Ordering::Relaxed);
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn owner_ptr(owner: *const Owner) -> *const u8 {
    unsafe { (*owner).view.bytes().as_ptr() }
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn owner_len(owner: *const Owner) -> usize {
    unsafe { (*owner).view.bytes().len() }
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn owner_safe(owner: *const Owner) -> bool {
    unsafe { (*owner).view.json_native_safe() }
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn owner_retained(owner: *const Owner) -> usize {
    unsafe { (*owner).view.retained_bytes() }
}

#[repr(C)]
pub struct Info {
    kind: u32,
    len: u32,
    integer: i64,
    real: f64,
    bytes: usize,
    node: usize,
}
static INFO: Mutex<Info> = Mutex::new(Info {
    kind: 0,
    len: 0,
    integer: 0,
    real: 0.0,
    bytes: 0,
    node: 0,
});
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn value_read(owner: *const Owner, node: usize) -> *const Info {
    let value = unsafe { (*owner).view.value_at(node) };
    let kind = value.kind();
    let len = if matches!(kind, sys::LITE3_TYPE_OBJECT | sys::LITE3_TYPE_ARRAY) {
        match value.len() {
            Ok(n) => n,
            Err(error) => {
                failure(error);
                return std::ptr::null();
            }
        }
    } else {
        value.span().map_or(0, <[u8]>::len)
    };
    let mut info = INFO.lock().unwrap();
    let public_kind = match kind {
        sys::LITE3_TYPE_NULL => 1,
        sys::LITE3_TYPE_BOOL => 2,
        sys::LITE3_TYPE_I64 => 3,
        sys::LITE3_TYPE_F64 => 4,
        sys::LITE3_TYPE_STRING => 5,
        sys::LITE3_TYPE_BYTES => 6,
        sys::LITE3_TYPE_OBJECT => 7,
        sys::LITE3_TYPE_ARRAY => 8,
        _ => unreachable!(),
    };
    *info = Info {
        kind: public_kind,
        len: len as u32,
        integer: value.integer().unwrap_or(0),
        real: value.real().unwrap_or(0.0),
        bytes: value.span().map_or(0, |s| s.as_ptr() as usize),
        node,
    };
    &*info
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn value_get(
    owner: *const Owner,
    node: usize,
    key: *const u8,
    len: usize,
) -> usize {
    let key = unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(key, len)) };
    result(
        unsafe { (*owner).view.value_at(node) }
            .get(key)
            .map(|value| value.map_or(usize::MAX, |v| v.offset())),
        usize::MAX - 1,
    )
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn value_index(owner: *const Owner, node: usize, index: u32) -> usize {
    result(
        unsafe { (*owner).view.value_at(node) }
            .index(index)
            .map(|value| value.map_or(usize::MAX, |v| v.offset())),
        usize::MAX - 1,
    )
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn value_json(owner: *const Owner, node: usize, stored: bool) -> i32 {
    let value = unsafe { (*owner).view.value_at(node) };
    let json = if stored {
        value.to_json_stored()
    } else {
        value.to_json()
    };
    result(
        json.map(|bytes| {
            *OUTPUT.lock().unwrap() = bytes;
            0
        }),
        -1,
    )
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Entry {
    key: usize,
    len: usize,
    node: usize,
}
pub struct Cursor {
    owner: *const Owner,
    state: view::NodeCursor,
    entries: [Entry; 16],
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn cursor_new(owner: *const Owner, node: usize) -> *mut Cursor {
    result(
        view::NodeCursor::new(unsafe { &(*owner).view }, node).map(|state| {
            Box::into_raw(Box::new(Cursor {
                owner,
                state,
                entries: [Entry::default(); 16],
            }))
        }),
        std::ptr::null_mut(),
    )
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn cursor_next(cursor: *mut Cursor) -> i32 {
    let cursor = unsafe { &mut *cursor };
    let mut count = 0;
    while count < 16 {
        match cursor.state.next(unsafe { &(*cursor.owner).view }) {
            Ok(Some((key, node, _))) => {
                cursor.entries[count] = Entry {
                    key: key.map_or(0, |s| s.as_ptr() as usize),
                    len: key.map_or(0, str::len),
                    node,
                };
                count += 1;
            }
            Ok(None) => break,
            Err(error) => {
                failure(error);
                return -1;
            }
        }
    }
    count as i32
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn cursor_entries(cursor: *const Cursor) -> *const Entry {
    unsafe { (*cursor).entries.as_ptr() }
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn cursor_free(cursor: *mut Cursor) {
    drop(unsafe { Box::from_raw(cursor) });
}
static LAST_NODE: AtomicUsize = AtomicUsize::new(0);
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn builder_new(tags: *const Text, count: usize) -> *mut plsm_builder {
    let (mut builder, mut node, mut error) = (std::ptr::null_mut(), 0, std::ptr::null_mut());
    let status = abi_builder::plsm_builder_new(tags, count, &mut builder, &mut node, &mut error);
    if checked(status, error) {
        LAST_NODE.store(node, Ordering::Relaxed);
        builder
    } else {
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn builder_data() -> usize {
    LAST_NODE.load(Ordering::Relaxed)
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn builder_put(
    builder: *mut plsm_builder,
    parent: usize,
    key: *const u8,
    key_len: usize,
    kind: u32,
    low: u32,
    high: i32,
    real: f64,
    bytes: *const u8,
    len: usize,
) -> usize {
    let (mut child, mut error) = (0, std::ptr::null_mut());
    let integer = (i64::from(high) << 32) | i64::from(low);
    let input = Input {
        kind,
        integer,
        real,
        bytes,
        len,
    };
    let status = abi_builder::plsm_builder_put(
        builder, parent, key, key_len, &input, &mut child, &mut error,
    );
    if checked(status, error) {
        child
    } else {
        usize::MAX
    }
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn builder_copy(
    builder: *mut plsm_builder,
    parent: usize,
    key: *const u8,
    key_len: usize,
    source: *const Owner,
    node: usize,
) -> i32 {
    let mut error = std::ptr::null_mut();
    let status = abi_builder::plsm_builder_copy_value(
        builder,
        parent,
        key,
        key_len,
        Ref {
            owner: source,
            offset: node,
        },
        &mut error,
    );
    if checked(status, error) { 0 } else { -1 }
}
#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn builder_finish(builder: *mut plsm_builder) -> *mut Owner {
    result(
        unsafe { &mut *builder }.take_view().map(own),
        std::ptr::null_mut(),
    )
}
