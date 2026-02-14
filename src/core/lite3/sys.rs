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

unsafe extern "C" {
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

    pub fn plasmite_lite3_count(
        buf: *const c_uchar,
        buf_len: usize,
        ofs: usize,
        out: *mut u32,
    ) -> c_int;

    pub fn plasmite_lite3_arr_get_type(
        buf: *const c_uchar,
        buf_len: usize,
        ofs: usize,
        index: u32,
        out_type: *mut c_uchar,
    ) -> c_int;

    pub fn plasmite_lite3_last_errno() -> c_int;

    pub fn plasmite_lite3_free(ptr: *mut c_void);
}
