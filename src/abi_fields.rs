//! Select requested values from an owned frame without building a data tree.
#![allow(clippy::result_large_err)]
use crate::abi::{borrow_pool, fail, plsm_buf, plsm_error, plsm_pool, write_json_buf};
use crate::core::error::{Error, ErrorKind};
use crate::core::lite3::{Lite3DocRef, sys};
use std::ffi::CStr;
use std::os::raw::c_char;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct plsm_field_value {
    kind: u32,
    reserved: u32,
    integer: i64,
    real: f64,
    offset: usize,
    len: usize,
}

#[repr(C)]
#[derive(Default)]
pub struct plsm_selected_frame {
    seq: u64,
    timestamp_ns: u64,
    flags: u32,
    reserved: u32,
}

fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage).with_message(message)
}

pub(crate) fn pointer_tokens(path: &str) -> Result<Vec<String>, Error> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let Some(path) = path.strip_prefix('/') else {
        return Err(usage("field path must be a JSON Pointer relative to data"));
    };
    path.split('/')
        .map(|part| {
            let mut text = String::with_capacity(part.len());
            let mut chars = part.chars();
            while let Some(c) = chars.next() {
                text.push(if c == '~' {
                    match chars.next() {
                        Some('0') => '~',
                        Some('1') => '/',
                        _ => return Err(usage("invalid JSON Pointer escape")),
                    }
                } else {
                    c
                });
            }
            Ok(text)
        })
        .collect()
}

fn view(doc: Lite3DocRef<'_>, offset: usize) -> sys::ValueView {
    let mut value = sys::ValueView::default();
    // All offsets come from the validated immutable frame or checked lookups.
    unsafe { sys::plasmite_lite3_value_view(doc.bytes().as_ptr(), offset, &mut value) };
    value
}

fn lookup(doc: Lite3DocRef<'_>, root: usize, tokens: &[String]) -> Result<Option<usize>, Error> {
    let mut offset = root;
    for token in tokens {
        match view(doc, offset).kind {
            sys::LITE3_TYPE_OBJECT => match doc.key_offset_at(offset, token) {
                Ok(next) => offset = next,
                Err(_) => return Ok(None),
            },
            sys::LITE3_TYPE_ARRAY => {
                if token.is_empty()
                    || (token.len() > 1 && token.starts_with('0'))
                    || !token.bytes().all(|c| c.is_ascii_digit())
                {
                    return Err(usage(
                        "array field path requires a nonnegative decimal index",
                    ));
                }
                let Ok(index) = token.parse::<u32>() else {
                    return Ok(None);
                };
                if index >= doc.count_at(offset)? {
                    return Ok(None);
                }
                let mut next = 0;
                let result = unsafe {
                    sys::plasmite_lite3_arr_get_val_ofs(
                        doc.bytes().as_ptr(),
                        doc.bytes().len(),
                        offset,
                        index,
                        &mut next,
                    )
                };
                if result < 0 {
                    return Err(Error::new(ErrorKind::Corrupt).with_message("invalid array value"));
                }
                offset = next;
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(offset))
}

fn project(payload: &[u8], paths: &[&str]) -> Result<(Vec<plsm_field_value>, Vec<u8>), Error> {
    // Wire bytes may start at any address. Request four-byte alignment only for
    // the fallback; ordinary owned frames already meet the native requirement.
    let aligned;
    let payload = if payload.as_ptr().align_offset(4) == 0 {
        payload
    } else {
        aligned = {
            let mut words = vec![0u32; payload.len().div_ceil(4)];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    payload.as_ptr(),
                    words.as_mut_ptr().cast(),
                    payload.len(),
                )
            };
            words
        };
        unsafe { std::slice::from_raw_parts(aligned.as_ptr().cast(), payload.len()) }
    };
    let doc = Lite3DocRef::new(payload);
    doc.validate()?;
    let root = doc.key_offset("data")?;
    let mut fields = Vec::with_capacity(paths.len());
    let mut bytes = Vec::new();
    for path in paths {
        let Some(offset) = lookup(doc, root, &pointer_tokens(path)?)? else {
            fields.push(plsm_field_value::default());
            continue;
        };
        let native = view(doc, offset);
        let mut field = plsm_field_value::default();
        field.kind = match native.kind {
            sys::LITE3_TYPE_NULL => 1,
            sys::LITE3_TYPE_BOOL => {
                field.integer = native.integer;
                2
            }
            sys::LITE3_TYPE_I64 => {
                field.integer = native.integer;
                3
            }
            sys::LITE3_TYPE_F64 => {
                field.real = native.real;
                4
            }
            sys::LITE3_TYPE_STRING | sys::LITE3_TYPE_BYTES => {
                field.offset = bytes.len();
                field.len = native.text_len;
                let source = unsafe {
                    std::slice::from_raw_parts(native.text.cast::<u8>(), native.text_len)
                };
                bytes.extend_from_slice(source);
                if native.kind == sys::LITE3_TYPE_STRING {
                    5
                } else {
                    6
                }
            }
            sys::LITE3_TYPE_OBJECT | sys::LITE3_TYPE_ARRAY => {
                field.integer = i64::from(doc.count_at(offset)?);
                if native.kind == sys::LITE3_TYPE_OBJECT {
                    7
                } else {
                    8
                }
            }
            _ => return Err(Error::new(ErrorKind::Corrupt).with_message("invalid selected value")),
        };
        fields.push(field);
    }
    Ok((fields, bytes))
}

pub(crate) unsafe fn paths<'a>(
    input: *const *const c_char,
    count: usize,
) -> Result<Vec<&'a str>, Error> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if input.is_null() {
        return Err(usage("paths is null"));
    }
    let input = unsafe { std::slice::from_raw_parts(input, count) };
    input
        .iter()
        .map(|&path| {
            if path.is_null() {
                return Err(usage("field path is null"));
            }
            unsafe { CStr::from_ptr(path) }
                .to_str()
                .map_err(|_| usage("field path is not UTF-8"))
        })
        .collect()
}

fn output(
    fields: Vec<plsm_field_value>,
    bytes: Vec<u8>,
    slots: *mut plsm_field_value,
    out_bytes: *mut plsm_buf,
) -> Result<(), Error> {
    write_json_buf(out_bytes, bytes)?;
    if !fields.is_empty() {
        unsafe { std::ptr::copy_nonoverlapping(fields.as_ptr(), slots, fields.len()) };
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_lite3_select(
    payload: *const u8,
    len: usize,
    input_paths: *const *const c_char,
    count: usize,
    out_values: *mut plsm_field_value,
    out_bytes: *mut plsm_buf,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let result = (|| {
        if payload.is_null() || out_bytes.is_null() || (count > 0 && out_values.is_null()) {
            return Err(usage(
                "payload, output buffer and requested slots must be non-null",
            ));
        }
        let paths = unsafe { paths(input_paths, count)? };
        let payload = unsafe { std::slice::from_raw_parts(payload, len) };
        let (values, bytes) = project(payload, &paths)?;
        output(values, bytes, out_values, out_bytes)
    })();
    match result {
        Ok(()) => 0,
        Err(error) => fail(out_err, error),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_get_fields(
    pool: *mut plsm_pool,
    seq: u64,
    input_paths: *const *const c_char,
    count: usize,
    out_values: *mut plsm_field_value,
    out_bytes: *mut plsm_buf,
    out_frame: *mut plsm_selected_frame,
    out_err: *mut *mut plsm_error,
) -> i32 {
    let pool = match borrow_pool(pool, out_err) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let result = (|| {
        if out_bytes.is_null() || out_frame.is_null() || (count > 0 && out_values.is_null()) {
            return Err(usage(
                "output buffer, frame and requested slots must be non-null",
            ));
        }
        let paths = unsafe { paths(input_paths, count)? };
        let frame = pool.pool.get(seq)?;
        let (values, bytes) = project(&frame.payload, &paths)?;
        output(values, bytes, out_values, out_bytes)?;
        unsafe {
            *out_frame = plsm_selected_frame {
                seq: frame.seq,
                timestamp_ns: frame.timestamp_ns,
                flags: frame.flags,
                reserved: 0,
            }
        };
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => fail(out_err, error),
    }
}
