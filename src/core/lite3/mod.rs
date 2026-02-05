//! Purpose: Safe wrappers around Lite3 encoding/decoding and canonical message validation.
//! Exports: `Lite3Buf`, `Lite3DocRef`, `encode_message`, `validate_bytes`.
//! Role: Canonical JSON <-> Lite3 boundary for payloads stored in pool frames.
//! Invariants: Buffer growth is capped (`MAX_LITE3_BUF`) to avoid unbounded allocation.
//! Invariants: All FFI interaction is confined to this module + `sys`.
use std::ffi::CString;
use std::io;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Map, Value};

use crate::core::error::{Error, ErrorKind};

pub mod sys;

const MAX_LITE3_BUF: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Lite3Buf {
    bytes: Vec<u8>,
}

impl Lite3Buf {
    pub fn from_json_str(json: &str) -> Result<Self, Error> {
        let mut buf_len = json.len().saturating_mul(2).max(256);
        let json_cstr = CString::new(json).map_err(|err| {
            Error::new(ErrorKind::Usage)
                .with_message("json contains null")
                .with_source(err)
        })?;

        loop {
            if buf_len > MAX_LITE3_BUF {
                return Err(
                    Error::new(ErrorKind::Usage).with_message("lite3 buffer exceeded max size")
                );
            }

            let mut buf = vec![0u8; buf_len];
            let mut out_len: usize = 0;
            let ret = unsafe {
                sys::plasmite_lite3_json_dec(
                    json_cstr.as_ptr(),
                    json.len(),
                    buf.as_mut_ptr(),
                    &mut out_len as *mut usize,
                    buf.len(),
                )
            };

            if ret == 0 {
                buf.truncate(out_len);
                return Ok(Self { bytes: buf });
            }

            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENOBUFS) {
                buf_len = buf_len.saturating_mul(2);
                continue;
            }

            return Err(Error::new(ErrorKind::Usage)
                .with_message("failed to encode json as lite3")
                .with_source(err));
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn as_doc(&self) -> Lite3DocRef<'_> {
        Lite3DocRef { bytes: &self.bytes }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Lite3DocRef<'a> {
    bytes: &'a [u8],
}

impl<'a> Lite3DocRef<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn to_json(&self, pretty: bool) -> Result<String, Error> {
        #[cfg(test)]
        TO_JSON_CALLS.fetch_add(1, Ordering::Relaxed);
        self.to_json_at(0, pretty)
    }

    pub fn to_json_at(&self, ofs: usize, pretty: bool) -> Result<String, Error> {
        #[cfg(test)]
        TO_JSON_AT_CALLS.fetch_add(1, Ordering::Relaxed);
        let mut out_len: usize = 0;
        let ptr = unsafe {
            if pretty {
                sys::plasmite_lite3_json_enc_pretty(
                    self.bytes.as_ptr(),
                    self.bytes.len(),
                    ofs,
                    &mut out_len as *mut usize,
                )
            } else {
                sys::plasmite_lite3_json_enc(
                    self.bytes.as_ptr(),
                    self.bytes.len(),
                    ofs,
                    &mut out_len as *mut usize,
                )
            }
        };

        if ptr.is_null() {
            return Err(Error::new(ErrorKind::Corrupt).with_message("failed to decode lite3"));
        }

        let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, out_len) };
        let json = String::from_utf8(slice.to_vec()).map_err(|err| {
            Error::new(ErrorKind::Corrupt)
                .with_message("invalid utf-8")
                .with_source(err)
        });

        unsafe {
            sys::plasmite_lite3_free(ptr as *mut libc::c_void);
        }

        json
    }

    pub fn key_offset(&self, key: &str) -> Result<usize, Error> {
        get_key_offset(self.bytes, key)
    }

    pub fn key_offset_at(&self, ofs: usize, key: &str) -> Result<usize, Error> {
        get_key_offset_at(self.bytes, ofs, key)
    }

    pub fn type_at_key(&self, ofs: usize, key: &str) -> Result<u8, Error> {
        let value = unsafe {
            sys::plasmite_lite3_get_type(
                self.bytes.as_ptr(),
                self.bytes.len(),
                ofs,
                c_key(key).as_ptr(),
            )
        };
        if value == sys::LITE3_TYPE_INVALID {
            return Err(Error::new(ErrorKind::Corrupt).with_message("missing key"));
        }
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), Error> {
        let root_type =
            unsafe { sys::plasmite_lite3_get_root_type(self.bytes.as_ptr(), self.bytes.len()) };
        if root_type != sys::LITE3_TYPE_OBJECT {
            return Err(Error::new(ErrorKind::Corrupt).with_message("root is not object"));
        }

        let meta_type = unsafe {
            sys::plasmite_lite3_get_type(
                self.bytes.as_ptr(),
                self.bytes.len(),
                0,
                c_key("meta").as_ptr(),
            )
        };
        if meta_type != sys::LITE3_TYPE_OBJECT {
            return Err(Error::new(ErrorKind::Corrupt).with_message("meta is not object"));
        }

        let data_type = unsafe {
            sys::plasmite_lite3_get_type(
                self.bytes.as_ptr(),
                self.bytes.len(),
                0,
                c_key("data").as_ptr(),
            )
        };
        if data_type != sys::LITE3_TYPE_OBJECT {
            return Err(Error::new(ErrorKind::Corrupt).with_message("data is not object"));
        }

        let meta_ofs = match get_key_offset(self.bytes, "meta") {
            Ok(ofs) => ofs,
            Err(err) => return Err(err.with_message("missing meta")),
        };

        let descrips_type = unsafe {
            sys::plasmite_lite3_get_type(
                self.bytes.as_ptr(),
                self.bytes.len(),
                meta_ofs,
                c_key("descrips").as_ptr(),
            )
        };
        if descrips_type != sys::LITE3_TYPE_ARRAY {
            return Err(Error::new(ErrorKind::Corrupt).with_message("meta.descrips must be array"));
        }

        let descrips_ofs = get_key_offset_at(self.bytes, meta_ofs, "descrips")
            .map_err(|err| err.with_message("missing meta.descrips"))?;

        let count = array_count(self.bytes, descrips_ofs)?;
        for index in 0..count {
            let item_type = array_item_type(self.bytes, descrips_ofs, index)?;
            if item_type != sys::LITE3_TYPE_STRING {
                return Err(Error::new(ErrorKind::Corrupt)
                    .with_message("meta.descrips must be string array"));
            }
        }

        Ok(())
    }
}

pub fn encode_message(meta_descrips: &[String], data: &Value) -> Result<Lite3Buf, Error> {
    if !matches!(data, Value::Object(_)) {
        return Err(Error::new(ErrorKind::Usage).with_message("data must be object"));
    }

    let descrips = meta_descrips
        .iter()
        .cloned()
        .map(Value::String)
        .collect::<Vec<_>>();

    let mut meta = Map::new();
    meta.insert("descrips".to_string(), Value::Array(descrips));

    let mut root = Map::new();
    root.insert("meta".to_string(), Value::Object(meta));
    root.insert("data".to_string(), data.clone());

    let json = Value::Object(root);
    let json_str = serde_json::to_string(&json).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("failed to serialize json")
            .with_source(err)
    })?;

    Lite3Buf::from_json_str(&json_str)
}

pub fn validate_bytes(buf: &[u8]) -> Result<(), Error> {
    Lite3DocRef::new(buf).validate()
}

fn c_key(key: &str) -> CString {
    CString::new(key).expect("c key")
}

fn get_key_offset(bytes: &[u8], key: &str) -> Result<usize, Error> {
    get_key_offset_at(bytes, 0, key)
}

fn get_key_offset_at(bytes: &[u8], ofs: usize, key: &str) -> Result<usize, Error> {
    let mut out_ofs: usize = 0;
    let ret = unsafe {
        sys::plasmite_lite3_get_val_ofs(
            bytes.as_ptr(),
            bytes.len(),
            ofs,
            c_key(key).as_ptr(),
            &mut out_ofs as *mut usize,
        )
    };
    if ret < 0 {
        return Err(Error::new(ErrorKind::Corrupt).with_message("missing key"));
    }
    Ok(out_ofs)
}

fn array_count(bytes: &[u8], ofs: usize) -> Result<u32, Error> {
    let mut out: u32 = 0;
    let ret = unsafe {
        sys::plasmite_lite3_count(bytes.as_ptr(), bytes.len(), ofs, &mut out as *mut u32)
    };
    if ret < 0 {
        return Err(Error::new(ErrorKind::Corrupt).with_message("invalid array"));
    }
    Ok(out)
}

fn array_item_type(bytes: &[u8], ofs: usize, index: u32) -> Result<u8, Error> {
    let mut out_type: u8 = 0;
    let ret = unsafe {
        sys::plasmite_lite3_arr_get_type(
            bytes.as_ptr(),
            bytes.len(),
            ofs,
            index,
            &mut out_type as *mut u8,
        )
    };
    if ret < 0 {
        return Err(Error::new(ErrorKind::Corrupt).with_message("invalid array index"));
    }
    Ok(out_type)
}

#[cfg(test)]
static TO_JSON_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TO_JSON_AT_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn reset_json_counters() {
    TO_JSON_CALLS.store(0, Ordering::Relaxed);
    TO_JSON_AT_CALLS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn json_counter_snapshot() -> (usize, usize) {
    (
        TO_JSON_CALLS.load(Ordering::Relaxed),
        TO_JSON_AT_CALLS.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::{Lite3Buf, encode_message, validate_bytes};
    use serde_json::json;

    #[test]
    fn round_trip_json() {
        let data = json!({"hello": "world"});
        let buf = encode_message(&["event".to_string()], &data).expect("encode");
        let doc = buf.as_doc();
        let json = doc.to_json(false).expect("json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(value["data"]["hello"], "world");
        assert_eq!(value["meta"]["descrips"][0], "event");
    }

    #[test]
    fn invalid_bytes_are_rejected() {
        let buf = [0u8; 8];
        let err = validate_bytes(&buf).expect_err("should fail");
        assert_eq!(err.kind(), crate::core::error::ErrorKind::Corrupt);
    }

    #[test]
    fn canonical_shape_is_required() {
        let json = r#"{"foo": "bar"}"#;
        let buf = Lite3Buf::from_json_str(json).expect("lite3");
        let err = validate_bytes(buf.as_slice()).expect_err("should fail");
        assert_eq!(err.kind(), crate::core::error::ErrorKind::Corrupt);
    }
}
