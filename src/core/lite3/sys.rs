//! Purpose: Raw (unsafe) FFI bindings to the Lite3 C shim.
//! Exports: `plasmite_lite3_*` functions and Lite3 type constants.
//! Role: Unsafe boundary used only by `crate::core::lite3`; keep surface minimal.
//! Invariants: Callers must pass valid pointers/lengths for the duration of the call.
//! Invariants: Any heap pointer returned from the shim is freed via `plasmite_lite3_free`.
use std::os::raw::{c_char, c_int, c_uchar, c_void};

pub const LITE3_TYPE_NULL: u8 = 0;
pub const LITE3_TYPE_BOOL: u8 = 1;
pub const LITE3_TYPE_I64: u8 = 2;
pub const LITE3_TYPE_F64: u8 = 3;
pub const LITE3_TYPE_BYTES: u8 = 4;
pub const LITE3_TYPE_STRING: u8 = 5;
pub const LITE3_TYPE_OBJECT: u8 = 6;
pub const LITE3_TYPE_ARRAY: u8 = 7;
pub const LITE3_TYPE_INVALID: u8 = 8;
pub(crate) const JSON_TYPE_U64: u8 = 9;

#[repr(C)]
pub(crate) struct TextRef {
    pub ptr: *const c_char,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct Entry {
    pub key: *const c_char,
    pub key_len: usize,
    pub offset: usize,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct ValueView {
    pub kind: u8,
    pub integer: i64,
    pub real: f64,
    pub text: *const c_char,
    pub text_len: usize,
}

unsafe extern "C" {
    pub(crate) fn plasmite_lite3_lookup_span(
        buf: *const u8,
        len: usize,
        ofs: usize,
        key: *const c_char,
        key_len: usize,
        out: *mut usize,
    ) -> c_int;
    pub(crate) fn plasmite_lite3_cursor_init(
        buf: *const u8,
        len: usize,
        ofs: usize,
        state: *mut c_void,
        size: usize,
    ) -> c_int;
    pub(crate) fn plasmite_lite3_cursor_next(
        buf: *const u8,
        len: usize,
        state: *mut c_void,
        out: *mut Entry,
        hash: *mut u32,
    ) -> c_int;
    pub(crate) fn plasmite_lite3_put(
        buf: *mut u8,
        len: *mut usize,
        capacity: usize,
        parent: usize,
        key: *const c_char,
        value: *const ValueView,
        child: *mut usize,
    ) -> c_int;

    pub(crate) fn plasmite_json_root(doc: *mut c_void) -> usize;
    pub(crate) fn plasmite_json_value_view(value: usize, out: *mut ValueView);
    pub(crate) fn plasmite_json_count(value: usize) -> usize;
    pub(crate) fn plasmite_json_entries(value: usize, out: *mut Entry, count: usize) -> c_int;
    pub(crate) fn plasmite_json_message_build(
        doc: *mut c_void,
        tags: *const TextRef,
        count: usize,
        buf: *mut u8,
        out_len: *mut usize,
        capacity: usize,
    ) -> c_int;
    pub(crate) fn plasmite_lite3_entries(
        buf: *const u8,
        len: usize,
        ofs: usize,
        out: *mut Entry,
        count: usize,
    ) -> c_int;
    pub(crate) fn plasmite_lite3_value_view(buf: *const u8, ofs: usize, out: *mut ValueView);
    pub fn plasmite_lite3_verify_json(buf: *const c_uchar, len: usize, ofs: usize) -> c_int;
    pub fn plasmite_lite3_json_parse(
        json: *const c_char,
        len: usize,
        values: *mut usize,
    ) -> *mut std::ffi::c_void;
    pub(crate) fn plasmite_lite3_json_parse_view(
        json: *const c_char,
        len: usize,
        values: *mut usize,
    ) -> *mut c_void;
    pub fn plasmite_lite3_json_doc_free(doc: *mut std::ffi::c_void);
    pub fn plasmite_lite3_json_build(
        doc: *mut std::ffi::c_void,
        buf: *mut c_uchar,
        out_len: *mut usize,
        buf_sz: usize,
    ) -> c_int;

    pub fn plasmite_lite3_json_dec(
        json_str: *const c_char,
        json_len: usize,
        buf: *mut c_uchar,
        out_len: *mut usize,
        buf_sz: usize,
    ) -> c_int;

    pub fn plasmite_lite3_json_enc(
        buf: *const c_uchar,
        buf_len: usize,
        ofs: usize,
        out_len: *mut usize,
    ) -> *mut c_char;

    pub fn plasmite_lite3_json_enc_pretty(
        buf: *const c_uchar,
        buf_len: usize,
        ofs: usize,
        out_len: *mut usize,
    ) -> *mut c_char;

    pub fn plasmite_lite3_get_root_type(buf: *const c_uchar, buf_len: usize) -> c_uchar;

    pub fn plasmite_lite3_get_type(
        buf: *const c_uchar,
        buf_len: usize,
        ofs: usize,
        key: *const c_char,
    ) -> c_uchar;

    pub fn plasmite_lite3_get_val_ofs(
        buf: *const c_uchar,
        buf_len: usize,
        ofs: usize,
        key: *const c_char,
        out_ofs: *mut usize,
    ) -> c_int;

    pub fn plasmite_lite3_get_bool(
        buf: *const c_uchar,
        buf_len: usize,
        ofs: usize,
        key: *const c_char,
        out: *mut bool,
    ) -> c_int;

    pub fn plasmite_lite3_get_i64(
        buf: *const c_uchar,
        buf_len: usize,
        ofs: usize,
        key: *const c_char,
        out: *mut i64,
    ) -> c_int;

    pub fn plasmite_lite3_count(
        buf: *const c_uchar,
        buf_len: usize,
        ofs: usize,
        out: *mut u32,
    ) -> c_int;

    pub(crate) fn plasmite_lite3_arr_get_val_ofs(
        buf: *const u8,
        len: usize,
        ofs: usize,
        index: u32,
        out: *mut usize,
    ) -> c_int;

    pub fn plasmite_lite3_arr_get_type(
        buf: *const c_uchar,
        buf_len: usize,
        ofs: usize,
        index: u32,
        out_type: *mut c_uchar,
    ) -> c_int;

    pub fn plasmite_lite3_arr_get_str(
        buf: *const c_uchar,
        buf_len: usize,
        ofs: usize,
        index: u32,
        out_ptr: *mut *const c_char,
        out_len: *mut usize,
    ) -> c_int;

    pub fn plasmite_lite3_last_errno() -> c_int;

    pub fn plasmite_lite3_free(ptr: *mut c_void);
}
