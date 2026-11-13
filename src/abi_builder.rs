//! Native runtime walkers construct one checked encoding through narrow C calls.
#![allow(clippy::result_large_err)] // Keep the shared structured Error ABI.
use crate::abi::{borrow_pool, fail, plsm_error, plsm_pool};
use crate::abi_views::{plsm_message_view, publish};
use crate::core::error::{Error, ErrorKind};
use crate::core::lite3::{builder::Builder, sys};

#[allow(non_camel_case_types)]
pub struct plsm_builder {
    value: Option<Builder>,
}
#[repr(C)]
pub struct Text {
    data: *const u8,
    len: usize,
}
#[repr(C)]
pub struct Input {
    kind: u32,
    integer: i64,
    real: f64,
    bytes: *const u8,
    len: usize,
}
fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage).with_message(message)
}
unsafe fn span<'a>(data: *const u8, len: usize) -> Result<&'a [u8], Error> {
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() {
        return Err(usage("null input span"));
    }
    Ok(unsafe { std::slice::from_raw_parts(data, len) })
}
fn text(bytes: &[u8]) -> Result<&str, Error> {
    std::str::from_utf8(bytes).map_err(|_| usage("input is not UTF-8"))
}
fn finish(result: Result<(), Error>, err: *mut *mut plsm_error) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => fail(err, e),
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_builder_new(
    tags: *const Text,
    count: usize,
    out: *mut *mut plsm_builder,
    data_node: *mut usize,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            if out.is_null() || data_node.is_null() || (count > 0 && tags.is_null()) {
                return Err(usage("null builder argument"));
            }
            let tags = if count == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(tags, count) }
            };
            let tags = tags
                .iter()
                .map(|s| text(unsafe { span(s.data, s.len) }?))
                .collect::<Result<Vec<_>, _>>()?;
            let builder = Builder::new(&tags)?;
            unsafe {
                *data_node = builder.data;
                *out = Box::into_raw(Box::new(plsm_builder {
                    value: Some(builder),
                }));
            }
            Ok(())
        })(),
        err,
    )
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_builder_put(
    builder: *mut plsm_builder,
    parent: usize,
    key: *const u8,
    key_len: usize,
    value: *const Input,
    child: *mut usize,
    err: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            let builder = unsafe { builder.as_mut() }
                .and_then(|b| b.value.as_mut())
                .ok_or_else(|| usage("builder is finished or null"))?;
            if parent == usize::MAX {
                return Err(usage("invalid parent node"));
            }
            let value = unsafe { value.as_ref() }.ok_or_else(|| usage("null input value"))?;
            let key = if key.is_null() {
                if key_len != 0 {
                    return Err(usage("null key span"));
                }
                None
            } else {
                Some(text(unsafe { span(key, key_len) }?)?)
            };
            let kind = match value.kind {
                1 => sys::LITE3_TYPE_NULL,
                2 => sys::LITE3_TYPE_BOOL,
                3 => sys::LITE3_TYPE_I64,
                4 => sys::LITE3_TYPE_F64,
                5 => sys::LITE3_TYPE_STRING,
                6 => sys::LITE3_TYPE_BYTES,
                7 => sys::LITE3_TYPE_OBJECT,
                8 => sys::LITE3_TYPE_ARRAY,
                _ => return Err(usage("unsupported input kind")),
            };
            if kind == sys::LITE3_TYPE_F64 && !value.real.is_finite() {
                return Err(usage("number must be finite"));
            }
            let bytes = if kind == sys::LITE3_TYPE_STRING || kind == sys::LITE3_TYPE_BYTES {
                unsafe { span(value.bytes, value.len) }?
            } else {
                &[]
            };
            if kind == sys::LITE3_TYPE_STRING {
                text(bytes)?;
            }
            let offset = builder.put(
                parent,
                key,
                sys::ValueView {
                    kind,
                    integer: value.integer,
                    real: value.real,
                    text: bytes.as_ptr().cast(),
                    text_len: bytes.len(),
                },
            )?;
            if !child.is_null() {
                unsafe {
                    *child = offset;
                }
            }
            Ok(())
        })(),
        err,
    )
}
#[unsafe(no_mangle)]
pub extern "C" fn plsm_pool_append_builder(
    pool: *mut plsm_pool,
    builder: *mut plsm_builder,
    durability: u32,
    out: *mut *const plsm_message_view,
    err: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() || builder.is_null() {
        return fail(err, usage("null append argument"));
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
                _ => return Err(usage("invalid durability")),
            };
            let builder = unsafe { &mut *builder }
                .value
                .take()
                .ok_or_else(|| usage("builder already finished"))?;
            let mut view = builder.finish()?;
            let time = crate::api::now_ns()?;
            view.seq = pool.pool.append_with_options(
                view.bytes(),
                crate::api::AppendOptions::new(time, durability),
            )?;
            view.timestamp_ns = time;
            publish(view, out)
        })(),
        err,
    )
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_builder_free(builder: *mut plsm_builder) {
    if !builder.is_null() {
        drop(unsafe { Box::from_raw(builder) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi_views::{plsm_view_bytes, plsm_view_release, plsm_view_seq};
    use crate::core::pool::{Pool, PoolOptions};
    fn input(kind: u32) -> Input {
        Input {
            kind,
            integer: 0,
            real: 0.0,
            bytes: std::ptr::null(),
            len: 0,
        }
    }
    #[test]
    fn native_builder_copies_inputs_and_returns_retained_committed_view() {
        let temp = tempfile::tempdir().unwrap();
        let pool = Box::into_raw(Box::new(plsm_pool {
            pool: Pool::create(
                temp.path().join("native.plasmite"),
                PoolOptions::new(1024 * 1024),
            )
            .unwrap(),
        }));
        let mut tag = b"sensor\0a".to_vec();
        let tags = Text {
            data: tag.as_ptr(),
            len: tag.len(),
        };
        let mut builder = std::ptr::null_mut();
        let mut data = 0;
        let mut err = std::ptr::null_mut();
        assert_eq!(
            plsm_builder_new(&tags, 1, &mut builder, &mut data, &mut err),
            0
        );
        tag[0] = b'X';
        let mut number = input(3);
        number.integer = 7;
        assert_eq!(
            plsm_builder_put(
                builder,
                data,
                b"count".as_ptr(),
                5,
                &number,
                std::ptr::null_mut(),
                &mut err
            ),
            0
        );
        let mut array = 0;
        assert_eq!(
            plsm_builder_put(
                builder,
                data,
                b"samples".as_ptr(),
                7,
                &input(8),
                &mut array,
                &mut err
            ),
            0
        );
        let mut bytes = b"a\0b".to_vec();
        let mut string = input(5);
        string.bytes = bytes.as_ptr();
        string.len = bytes.len();
        assert_eq!(
            plsm_builder_put(
                builder,
                array,
                std::ptr::null(),
                0,
                &string,
                std::ptr::null_mut(),
                &mut err
            ),
            0
        );
        bytes[0] = b'X';
        let mut view = std::ptr::null();
        assert_eq!(
            plsm_pool_append_builder(pool, builder, 0, &mut view, &mut err),
            0
        );
        assert_eq!(unsafe { plsm_view_seq(view) }, 1);
        unsafe {
            plsm_builder_free(builder);
        }
        crate::abi::plsm_pool_free(pool);
        let mut len = 0;
        let ptr = unsafe { plsm_view_bytes(view, &mut len) };
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        let json = crate::core::lite3::Lite3DocRef::new(bytes)
            .to_json(false)
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["meta"]["tags"][0], "sensor\0a");
        assert_eq!(value["data"]["count"], 7);
        assert_eq!(value["data"]["samples"][0], "a\0b");
        unsafe {
            plsm_view_release(view);
        }
    }
    #[test]
    fn native_builder_rejects_invalid_keys_and_values() {
        let mut builder = std::ptr::null_mut();
        let mut data = 0;
        let mut err = std::ptr::null_mut();
        assert_eq!(
            plsm_builder_new(std::ptr::null(), 0, &mut builder, &mut data, &mut err),
            0
        );
        let mut number = input(4);
        number.real = f64::NAN;
        assert_eq!(
            plsm_builder_put(
                builder,
                data,
                b"x".as_ptr(),
                1,
                &number,
                std::ptr::null_mut(),
                &mut err
            ),
            -1
        );
        crate::abi::plsm_error_free(err);
        err = std::ptr::null_mut();
        assert_eq!(
            plsm_builder_put(
                builder,
                data,
                b"bad\0key".as_ptr(),
                7,
                &input(1),
                std::ptr::null_mut(),
                &mut err
            ),
            -1
        );
        crate::abi::plsm_error_free(err);
        unsafe {
            plsm_builder_free(builder);
        }
    }
}
