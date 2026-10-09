//! Purpose: Safe wrappers around Lite3 encoding/decoding and canonical message validation.
//! Exports: `Lite3Buf`, `Lite3DocRef`, `encode_message`, `validate_bytes`.
//! Role: Canonical JSON <-> Lite3 boundary for payloads stored in pool frames.
//! Invariants: Buffer growth is capped (`MAX_LITE3_BUF`) to avoid unbounded allocation.
//! Invariants: All FFI interaction is confined to this module + `sys`.
#[cfg(test)]
use std::cell::Cell;
use std::ffi::CString;
use std::io;

use serde::Serialize;
use serde_json::Value;

use crate::core::error::{Error, ErrorKind};

pub mod sys;

/// Maximum buffer size for encoded messages and single-document CLI input.
pub const MAX_LITE3_BUF: usize = 256 * 1024 * 1024;

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

            let err_no = unsafe { sys::plasmite_lite3_last_errno() };
            if err_no == libc::ENOBUFS {
                buf_len = buf_len.saturating_mul(2);
                continue;
            }

            let err = if err_no != 0 {
                io::Error::from_raw_os_error(err_no)
            } else {
                io::Error::other("unknown lite3 errno")
            };

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
        TO_JSON_CALLS.with(|count| count.set(count.get() + 1));
        self.to_json_at(0, pretty)
    }

    pub fn to_json_at(&self, ofs: usize, pretty: bool) -> Result<String, Error> {
        #[cfg(test)]
        TO_JSON_AT_CALLS.with(|count| count.set(count.get() + 1));
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

    pub fn count_at(&self, ofs: usize) -> Result<u32, Error> {
        array_count(self.bytes, ofs)
    }

    pub fn array_item_type(&self, ofs: usize, index: u32) -> Result<u8, Error> {
        array_item_type(self.bytes, ofs, index)
    }

    pub fn array_string_at(&self, ofs: usize, index: u32) -> Result<String, Error> {
        let mut out_ptr: *const std::os::raw::c_char = std::ptr::null();
        let mut out_len: usize = 0;
        let ret = unsafe {
            sys::plasmite_lite3_arr_get_str(
                self.bytes.as_ptr(),
                self.bytes.len(),
                ofs,
                index,
                &mut out_ptr as *mut *const std::os::raw::c_char,
                &mut out_len as *mut usize,
            )
        };
        if ret < 0 || out_ptr.is_null() {
            return Err(
                Error::new(ErrorKind::Corrupt).with_message("missing or invalid array item")
            );
        }
        let bytes = unsafe { std::slice::from_raw_parts(out_ptr.cast::<u8>(), out_len) };
        let text = std::str::from_utf8(bytes).map_err(|err| {
            Error::new(ErrorKind::Corrupt)
                .with_message("invalid utf-8")
                .with_source(err)
        })?;
        Ok(text.to_string())
    }

    pub fn bool_at_key(&self, ofs: usize, key: &str) -> Result<bool, Error> {
        let mut out = false;
        let ret = unsafe {
            sys::plasmite_lite3_get_bool(
                self.bytes.as_ptr(),
                self.bytes.len(),
                ofs,
                c_key(key)?.as_ptr(),
                &mut out as *mut bool,
            )
        };
        if ret < 0 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("missing or invalid key"));
        }
        Ok(out)
    }

    pub fn i64_at_key(&self, ofs: usize, key: &str) -> Result<i64, Error> {
        let mut out: i64 = 0;
        let ret = unsafe {
            sys::plasmite_lite3_get_i64(
                self.bytes.as_ptr(),
                self.bytes.len(),
                ofs,
                c_key(key)?.as_ptr(),
                &mut out as *mut i64,
            )
        };
        if ret < 0 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("missing or invalid key"));
        }
        Ok(out)
    }

    pub fn type_at_key(&self, ofs: usize, key: &str) -> Result<u8, Error> {
        let value = unsafe {
            sys::plasmite_lite3_get_type(
                self.bytes.as_ptr(),
                self.bytes.len(),
                ofs,
                c_key(key)?.as_ptr(),
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
                c_key("meta")?.as_ptr(),
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
                c_key("data")?.as_ptr(),
            )
        };
        if data_type != sys::LITE3_TYPE_OBJECT {
            return Err(Error::new(ErrorKind::Corrupt).with_message("data is not object"));
        }

        let meta_ofs = match get_key_offset(self.bytes, "meta") {
            Ok(ofs) => ofs,
            Err(err) => return Err(err.with_message("missing meta")),
        };

        let tags_type = unsafe {
            sys::plasmite_lite3_get_type(
                self.bytes.as_ptr(),
                self.bytes.len(),
                meta_ofs,
                c_key("tags")?.as_ptr(),
            )
        };
        if tags_type != sys::LITE3_TYPE_ARRAY {
            return Err(Error::new(ErrorKind::Corrupt).with_message("meta.tags must be array"));
        }

        let tags_ofs = get_key_offset_at(self.bytes, meta_ofs, "tags")
            .map_err(|err| err.with_message("missing meta.tags"))?;

        let count = array_count(self.bytes, tags_ofs)?;
        for index in 0..count {
            self.array_string_at(tags_ofs, index)
                .map_err(|err| err.with_message("meta.tags must be string array"))?;
        }

        // Outer fields can be valid while nested data cannot produce a readable
        // message. Check that data before any append can commit its bytes.
        let data_ofs = self.key_offset("data")?;
        self.to_json_at(data_ofs, false)?;

        Ok(())
    }
}

pub fn encode_message(meta_tags: &[String], data: &Value) -> Result<Lite3Buf, Error> {
    if !matches!(data, Value::Object(_)) {
        return Err(Error::new(ErrorKind::Usage).with_message("data must be object"));
    }

    #[derive(Serialize)]
    struct MetaEnvelope<'a> {
        tags: &'a [String],
    }

    #[derive(Serialize)]
    struct MessageEnvelope<'a> {
        meta: MetaEnvelope<'a>,
        data: &'a Value,
    }

    let json = MessageEnvelope {
        meta: MetaEnvelope { tags: meta_tags },
        data,
    };
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

fn c_key(key: &str) -> Result<CString, Error> {
    CString::new(key).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("key contains null")
            .with_source(err)
    })
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
            c_key(key)?.as_ptr(),
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
    // Each encoded element occupies bytes. A corrupt node count must not drive
    // decoder allocations or iteration beyond even this conservative bound.
    if u64::from(out) > bytes.len() as u64 {
        return Err(Error::new(ErrorKind::Corrupt).with_message("array count exceeds payload size"));
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
std::thread_local! {
    static TO_JSON_CALLS: Cell<usize> = const { Cell::new(0) };
    static TO_JSON_AT_CALLS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_json_counters() {
    TO_JSON_CALLS.with(|count| count.set(0));
    TO_JSON_AT_CALLS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn json_counter_snapshot() -> (usize, usize) {
    (
        TO_JSON_CALLS.with(Cell::get),
        TO_JSON_AT_CALLS.with(Cell::get),
    )
}

#[cfg(test)]
mod tests {
    use super::{Lite3Buf, Lite3DocRef, encode_message, validate_bytes};
    use serde_json::json;

    #[test]
    fn round_trip_json() {
        let data = json!({"hello": "world"});
        let buf = encode_message(&["event".to_string()], &data).expect("encode");
        let doc = buf.as_doc();
        let json = doc.to_json(false).expect("json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(value["data"]["hello"], "world");
        assert_eq!(value["meta"]["tags"][0], "event");
    }

    #[test]
    fn round_trip_empty_object_key_and_nested_array() {
        let value = json!({
            "": null,
            "nested": [{"values": [1, 2, 3]}]
        });
        let json = serde_json::to_string(&value).expect("serialize");
        let buf = Lite3Buf::from_json_str(&json).expect("encode");
        let decoded = buf.as_doc().to_json(false).expect("decode");
        let decoded: serde_json::Value = serde_json::from_str(&decoded).expect("parse");
        assert_eq!(decoded, value);
    }

    #[test]
    fn invalid_bytes_are_rejected() {
        let buf = [0u8; 8];
        let err = validate_bytes(&buf).expect_err("should fail");
        assert_eq!(err.kind(), crate::core::error::ErrorKind::Corrupt);
    }

    #[test]
    fn short_buffers_and_invalid_offsets_are_rejected() {
        let bytes = [0u8; 96];
        for len in 0..bytes.len() {
            let doc = Lite3DocRef::new(&bytes[..len]);
            assert!(doc.key_offset_at(0, "x").is_err(), "key at length {len}");
            assert!(doc.count_at(0).is_err(), "count at length {len}");
            assert!(doc.array_item_type(0, 0).is_err(), "type at length {len}");
            assert!(doc.array_string_at(0, 0).is_err(), "string at length {len}");
            assert!(doc.to_json(false).is_err(), "json at length {len}");
            assert!(doc.to_json(true).is_err(), "pretty json at length {len}");
            assert!(doc.bool_at_key(0, "x").is_err(), "bool at length {len}");
            assert!(doc.i64_at_key(0, "x").is_err(), "i64 at length {len}");
            assert!(doc.type_at_key(0, "x").is_err(), "type at length {len}");
            assert!(doc.validate().is_err(), "validate at length {len}");
        }

        let buf = encode_message(&["event".to_string()], &json!({"x": true})).expect("encode");
        let doc = buf.as_doc();
        for ofs in [buf.len(), usize::MAX] {
            assert!(doc.key_offset_at(ofs, "x").is_err());
            assert!(doc.count_at(ofs).is_err());
            assert!(doc.array_item_type(ofs, 0).is_err());
            assert!(doc.array_string_at(ofs, 0).is_err());
            assert!(doc.to_json_at(ofs, false).is_err());
            assert!(doc.bool_at_key(ofs, "x").is_err());
            assert!(doc.i64_at_key(ofs, "x").is_err());
            assert!(doc.type_at_key(ofs, "x").is_err());
        }
    }

    #[test]
    fn misaligned_buffer_is_rejected() {
        let buf = encode_message(&["event".to_string()], &json!({})).expect("encode");
        let mut bytes = Vec::with_capacity(buf.len() + 1);
        bytes.push(0);
        bytes.extend_from_slice(buf.as_slice());
        let doc = Lite3DocRef::new(&bytes[1..]);
        assert!(doc.key_offset("meta").is_err());
        assert!(doc.count_at(0).is_err());
        assert!(doc.to_json(false).is_err());
        assert!(doc.validate().is_err());
    }

    #[test]
    fn null_byte_in_lookup_key_returns_usage_error() {
        let buf = encode_message(&[], &json!({})).expect("encode");
        let doc = buf.as_doc();
        for result in [
            doc.key_offset("x\0y").map(|_| ()),
            doc.type_at_key(0, "x\0y").map(|_| ()),
            doc.bool_at_key(0, "x\0y").map(|_| ()),
            doc.i64_at_key(0, "x\0y").map(|_| ()),
        ] {
            assert_eq!(
                result.expect_err("invalid key").kind(),
                crate::core::error::ErrorKind::Usage
            );
        }
    }

    #[test]
    fn zero_length_encoded_string_is_rejected() {
        let buf = encode_message(&["event".to_string()], &json!({})).expect("encode");
        let doc = buf.as_doc();
        let meta = doc.key_offset("meta").expect("meta");
        let tags = doc.key_offset_at(meta, "tags").expect("tags");
        let mut bytes = buf.as_slice().to_vec();
        let value_ofs = u32::from_le_bytes(bytes[tags + 36..tags + 40].try_into().expect("offset"));
        let len_ofs = value_ofs as usize + 1;
        bytes[len_ofs..len_ofs + 4].copy_from_slice(&0u32.to_le_bytes());

        let doc = Lite3DocRef::new(&bytes);
        assert!(doc.array_string_at(tags, 0).is_err());
        assert!(doc.to_json(false).is_err());
        assert!(doc.validate().is_err());
    }

    #[test]
    fn malformed_key_terminator_is_rejected_before_json_encoding() {
        let buf = Lite3Buf::from_json_str(r#"{"x":true}"#).expect("encode");
        let mut bytes = buf.as_slice().to_vec();
        let suffix = bytes.len() - 4;
        assert_eq!(&bytes[suffix..], &[b'x', 0, 1, 1]);
        bytes[suffix + 1] = 0xff;

        let doc = Lite3DocRef::new(&bytes);
        assert!(doc.to_json(false).is_err());
        assert!(doc.to_json(true).is_err());
    }

    #[test]
    fn malformed_string_terminator_is_rejected_before_json_encoding() {
        let buf = Lite3Buf::from_json_str(r#"{"x":"y"}"#).expect("encode");
        let mut bytes = buf.as_slice().to_vec();
        let suffix = bytes.len() - 7;
        assert_eq!(&bytes[suffix..], &[5, 2, 0, 0, 0, b'y', 0]);
        bytes[suffix + 6] = 0xff;

        let doc = Lite3DocRef::new(&bytes);
        assert!(doc.to_json(false).is_err());
        assert!(doc.to_json(true).is_err());
    }

    #[test]
    fn corrupt_array_count_is_rejected_before_decoder_allocation() {
        let buf = encode_message(&[], &json!({"x": 1})).expect("encode");
        let doc = buf.as_doc();
        let meta = doc.key_offset("meta").expect("meta");
        let tags = doc.key_offset_at(meta, "tags").expect("tags");
        let mut bytes = buf.as_slice().to_vec();
        // Lite3 size_kc stores count in the high 26 bits and key count in the
        // low six. Keep the empty array's key count and corrupt only its size.
        bytes[tags + 32..tags + 36].copy_from_slice(&(u32::MAX & !63).to_le_bytes());
        let doc = super::Lite3DocRef::new(&bytes);
        let err = doc.count_at(tags).expect_err("impossible count");
        assert_eq!(err.kind(), crate::core::error::ErrorKind::Corrupt);
        assert_eq!(
            validate_bytes(&bytes)
                .expect_err("invalid canonical message")
                .kind(),
            crate::core::error::ErrorKind::Corrupt
        );
    }

    #[test]
    fn canonical_shape_is_required() {
        let json = r#"{"foo": "bar"}"#;
        let buf = Lite3Buf::from_json_str(json).expect("lite3");
        let err = validate_bytes(buf.as_slice()).expect_err("should fail");
        assert_eq!(err.kind(), crate::core::error::ErrorKind::Corrupt);
    }

    #[test]
    fn unreadable_nested_data_is_rejected_before_append() {
        use crate::api::{AppendOptions, Durability, ErrorKind, Pool, PoolApiExt, PoolOptions};

        let payload = encode_message(&[], &json!({"x": "y"})).expect("encode");
        let doc = payload.as_doc();
        let data = doc.key_offset("data").expect("data");
        let value = doc.key_offset_at(data, "x").expect("x");
        let mut bytes = payload.as_slice().to_vec();
        assert_eq!(bytes[value + 5], b'y');
        bytes[value + 5] = 0xff;

        let temp = tempfile::tempdir().expect("tempdir");
        let mut pool = Pool::create(
            temp.path().join("invalid.plasmite"),
            PoolOptions::new(1024 * 1024),
        )
        .expect("create");
        let before = pool.info().expect("info").bounds;
        let err = pool
            .append_lite3(&bytes, AppendOptions::new(123, Durability::Fast))
            .expect_err("invalid UTF-8 must not append");
        assert_eq!(err.kind(), ErrorKind::Corrupt);
        assert_eq!(pool.info().expect("info").bounds, before);
    }

    #[test]
    fn typed_key_getters_work() {
        let data = json!({"done": true, "sent_ns": 42});
        let buf = encode_message(&["event".to_string()], &data).expect("encode");
        let doc = buf.as_doc();
        let data_ofs = doc.key_offset("data").expect("data offset");
        let meta_ofs = doc.key_offset("meta").expect("meta offset");
        let tags_ofs = doc.key_offset_at(meta_ofs, "tags").expect("tags offset");
        assert!(doc.bool_at_key(data_ofs, "done").expect("done"));
        assert_eq!(doc.i64_at_key(data_ofs, "sent_ns").expect("sent_ns"), 42);
        assert_eq!(doc.count_at(tags_ofs).expect("tags count"), 1);
        assert_eq!(
            doc.array_string_at(tags_ofs, 0).expect("tag 0"),
            "event".to_string()
        );
    }
}
