// Lite3 safe wrappers for encoding/decoding and canonical message validation.
use std::ffi::CString;
use std::io;

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
        let json_cstr = CString::new(json)
            .map_err(|err| Error::new(ErrorKind::Usage).with_message("json contains null").with_source(err))?;

        loop {
            if buf_len > MAX_LITE3_BUF {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("lite3 buffer exceeded max size"));
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

    pub fn as_doc(&self) -> Lite3DocRef<'_> {
        Lite3DocRef { bytes: &self.bytes }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Lite3DocRef<'a> {
    bytes: &'a [u8],
}

impl<'a> Lite3DocRef<'a> {
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn to_json(&self, pretty: bool) -> Result<String, Error> {
        let mut out_len: usize = 0;
        let ptr = unsafe {
            if pretty {
                sys::plasmite_lite3_json_enc_pretty(
                    self.bytes.as_ptr(),
                    self.bytes.len(),
                    0,
                    &mut out_len as *mut usize,
                )
            } else {
                sys::plasmite_lite3_json_enc(
                    self.bytes.as_ptr(),
                    self.bytes.len(),
                    0,
                    &mut out_len as *mut usize,
                )
            }
        };

        if ptr.is_null() {
            return Err(Error::new(ErrorKind::Corrupt).with_message("failed to decode lite3"));
        }

        let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, out_len) };
        let json = String::from_utf8(slice.to_vec())
            .map_err(|err| Error::new(ErrorKind::Corrupt).with_message("invalid utf-8").with_source(err));

        unsafe {
            sys::plasmite_lite3_free(ptr as *mut libc::c_void);
        }

        json
    }

    pub fn validate(&self) -> Result<(), Error> {
        let root_type = unsafe { sys::plasmite_lite3_get_root_type(self.bytes.as_ptr(), self.bytes.len()) };
        if root_type != sys::LITE3_TYPE_OBJECT {
            return Err(Error::new(ErrorKind::Corrupt).with_message("root is not object"));
        }

        let json = self.to_json(false)?;
        let value: Value = serde_json::from_str(&json)
            .map_err(|err| Error::new(ErrorKind::Corrupt).with_message("invalid json").with_source(err))?;

        let obj = match value {
            Value::Object(map) => map,
            _ => return Err(Error::new(ErrorKind::Corrupt).with_message("root is not object")),
        };

        let meta = obj
            .get("meta")
            .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("missing meta"))?;
        let data = obj
            .get("data")
            .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("missing data"))?;

        let meta_obj = match meta {
            Value::Object(map) => map,
            _ => return Err(Error::new(ErrorKind::Corrupt).with_message("meta is not object")),
        };

        let descrips = meta_obj
            .get("descrips")
            .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("missing meta.descrips"))?;

        match descrips {
            Value::Array(items) => {
                if items.iter().any(|item| !matches!(item, Value::String(_))) {
                    return Err(Error::new(ErrorKind::Corrupt)
                        .with_message("meta.descrips must be string array"));
                }
            }
            _ => {
                return Err(
                    Error::new(ErrorKind::Corrupt).with_message("meta.descrips must be array")
                );
            }
        }

        if !matches!(data, Value::Object(_)) {
            return Err(Error::new(ErrorKind::Corrupt).with_message("data is not object"));
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
    let json_str = serde_json::to_string(&json)
        .map_err(|err| Error::new(ErrorKind::Usage).with_message("failed to serialize json").with_source(err))?;

    Lite3Buf::from_json_str(&json_str)
}

pub fn validate_bytes(buf: &[u8]) -> Result<(), Error> {
    Lite3DocRef { bytes: buf }.validate()
}

#[cfg(test)]
mod tests {
    use super::{encode_message, validate_bytes, Lite3Buf};
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
