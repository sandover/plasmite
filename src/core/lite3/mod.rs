//! Purpose: Safe wrappers around Lite3 encoding/decoding and canonical message validation.
//! Exports: `Lite3Buf`, `Lite3DocRef`, `encode_message`, `validate_bytes`.
//! Role: Canonical JSON <-> Lite3 boundary for payloads stored in pool frames.
//! Invariants: Buffer growth is capped (`MAX_LITE3_BUF`) to avoid unbounded allocation.
//! Invariants: All FFI interaction is confined to this module + `sys`.
#[cfg(test)]
use std::cell::Cell;
use std::ffi::{CStr, CString};
use std::io;

use serde::Serialize;
use serde_json::Value;

use crate::core::error::{Error, ErrorKind};

pub(crate) mod builder;
mod json;
pub mod sys;
pub(crate) use json::JsonValue;

/// Maximum buffer size for encoded messages and single-document CLI input.
pub const MAX_LITE3_BUF: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Lite3Buf {
    bytes: Vec<u8>,
}

// YYJSON_READ_NOFLAG gives the document its own storage; it borrows no input.
// This owner never escapes encoding and frees through the matching C allocator.
pub(crate) struct ParsedJson {
    ptr: std::ptr::NonNull<std::ffi::c_void>,
    source_len: usize,
    values: usize,
}

impl Drop for ParsedJson {
    fn drop(&mut self) {
        unsafe { sys::plasmite_lite3_json_doc_free(self.ptr.as_ptr()) };
    }
}

fn json_encoding_error() -> Error {
    let errno = unsafe { sys::plasmite_lite3_last_errno() };
    let source = if errno != 0 {
        io::Error::from_raw_os_error(errno)
    } else {
        io::Error::other("unknown lite3 errno")
    };
    Error::new(ErrorKind::Usage)
        .with_message("failed to encode json as lite3")
        .with_source(source)
}

#[cfg(test)]
thread_local! {
    // Parse calls, output attempts, total requested output bytes. No retained cache.
    static ENCODE_STATS: Cell<(usize, usize, usize)> = const { Cell::new((0, 0, 0)) };
}

impl Lite3Buf {
    pub fn from_json_str(json: &str) -> Result<Self, Error> {
        Self::from_json_str_with_limit(json, MAX_LITE3_BUF)
    }

    fn from_json_str_with_limit(json: &str, max_bytes: usize) -> Result<Self, Error> {
        ParsedJson::parse(json.as_bytes())?.build(max_bytes, None)
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

impl ParsedJson {
    pub(crate) fn parse(input: &[u8]) -> Result<Self, Error> {
        Self::parse_with(input, false)
    }
    pub(crate) fn parse_view(input: &[u8]) -> Result<Self, Error> {
        Self::parse_with(input, true)
    }
    fn parse_with(input: &[u8], strict: bool) -> Result<Self, Error> {
        if input.contains(&0) {
            return Err(Error::new(ErrorKind::Usage).with_message("json contains null"));
        }
        #[cfg(test)]
        ENCODE_STATS.with(|stats| {
            let (p, a, b) = stats.get();
            stats.set((p + 1, a, b));
        });
        let mut values = 0;
        let ptr = std::ptr::NonNull::new(unsafe {
            if strict {
                sys::plasmite_lite3_json_parse_view(input.as_ptr().cast(), input.len(), &mut values)
            } else {
                sys::plasmite_lite3_json_parse(input.as_ptr().cast(), input.len(), &mut values)
            }
        })
        .ok_or_else(json_encoding_error)?;
        Ok(Self {
            ptr,
            source_len: input.len(),
            values,
        })
    }

    pub(crate) fn value(&self) -> JsonValue<'_> {
        JsonValue::from_parsed(self)
    }

    pub(crate) fn encode_message(&self, tags: &[&str]) -> Result<Lite3Buf, Error> {
        if self.value().kind() != sys::LITE3_TYPE_OBJECT {
            return Err(Error::new(ErrorKind::Usage).with_message("data must be object"));
        }
        let tags: Vec<_> = tags
            .iter()
            .map(|tag| sys::TextRef {
                ptr: tag.as_ptr().cast(),
                len: tag.len(),
            })
            .collect();
        self.build(MAX_LITE3_BUF, Some(&tags))
    }

    pub(crate) fn encode_message_view(
        &self,
        tags: &[&str],
    ) -> Result<crate::view::MessageView, Error> {
        if self.value().kind() != sys::LITE3_TYPE_OBJECT {
            return Err(Error::new(ErrorKind::Usage).with_message("data must be object"));
        }
        let mut builder = builder::Builder::new(tags)?;
        builder.copy_parsed(unsafe { sys::plasmite_json_root(self.ptr.as_ptr()) })?;
        // Preparation and the copy walk checked text, finite/exact numbers,
        // object shape and envelope-relative depth; setters keep unique keys.
        unsafe { builder.finish_proven() }
    }

    fn build(&self, max_bytes: usize, tags: Option<&[sys::TextRef]>) -> Result<Lite3Buf, Error> {
        // Structural sizing includes the envelope and tags. Retain one parsed
        // document across capacity retries; every attempt starts with empty storage.
        let extra = tags.map_or(0, |t| {
            (t.len() + 6) * 32 + t.iter().map(|t| t.len).sum::<usize>()
        });
        let mut capacity = self
            .source_len
            .saturating_add(self.values.saturating_mul(32))
            .saturating_add(64)
            .saturating_add(extra)
            .max(256)
            .min(max_bytes);
        loop {
            #[cfg(test)]
            ENCODE_STATS.with(|stats| {
                let (p, a, b) = stats.get();
                stats.set((p, a + 1, b + capacity));
            });
            let mut bytes = vec![0; capacity];
            let mut len = 0;
            let result = unsafe {
                match tags {
                    Some(tags) => sys::plasmite_json_message_build(
                        self.ptr.as_ptr(),
                        tags.as_ptr(),
                        tags.len(),
                        bytes.as_mut_ptr(),
                        &mut len,
                        capacity,
                    ),
                    None => sys::plasmite_lite3_json_build(
                        self.ptr.as_ptr(),
                        bytes.as_mut_ptr(),
                        &mut len,
                        capacity,
                    ),
                }
            };
            if result == 0 {
                bytes.truncate(len);
                return Ok(Lite3Buf { bytes });
            }
            let errno = unsafe { sys::plasmite_lite3_last_errno() };
            if errno != libc::ENOBUFS {
                return Err(json_encoding_error());
            }
            if capacity == max_bytes {
                return Err(
                    Error::new(ErrorKind::Usage).with_message("lite3 buffer exceeded max size")
                );
            }
            capacity = capacity.saturating_mul(2).min(max_bytes);
        }
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

    pub(crate) fn key_offset_cstr_at(&self, ofs: usize, key: &CStr) -> Result<usize, Error> {
        get_key_offset_cstr_at(self.bytes, ofs, key)
    }

    pub fn count_at(&self, ofs: usize) -> Result<u32, Error> {
        array_count(self.bytes, ofs)
    }

    pub fn array_item_type(&self, ofs: usize, index: u32) -> Result<u8, Error> {
        array_item_type(self.bytes, ofs, index)
    }

    pub fn array_string_at(&self, ofs: usize, index: u32) -> Result<String, Error> {
        self.array_str_at(ofs, index).map(str::to_owned)
    }

    pub(crate) fn array_str_at(&self, ofs: usize, index: u32) -> Result<&'a str, Error> {
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
        Ok(text)
    }

    pub(crate) fn str_at_key(&self, ofs: usize, key: &CStr) -> Result<&'a str, Error> {
        let mut ptr = std::ptr::null();
        let mut len = 0;
        let result = unsafe {
            sys::plasmite_lite3_get_str(
                self.bytes.as_ptr(),
                self.bytes.len(),
                ofs,
                key.as_ptr(),
                &mut ptr,
                &mut len,
            )
        };
        if result < 0 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("missing or invalid string"));
        }
        let bytes = unsafe { std::slice::from_raw_parts(ptr.cast(), len) };
        std::str::from_utf8(bytes).map_err(|err| Error::new(ErrorKind::Corrupt).with_source(err))
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

    pub(crate) fn validate_readability_at(&self, ofs: usize) -> Result<(), Error> {
        match unsafe { sys::plasmite_lite3_verify_json(self.bytes.as_ptr(), self.bytes.len(), ofs) }
        {
            0 => Ok(()),
            1 => self.to_json_at(ofs, false).map(|_| ()),
            _ => Err(Error::new(ErrorKind::Corrupt).with_message("failed to decode lite3")),
        }
    }

    fn validate_shape(&self, data_kind: ErrorKind) -> Result<usize, Error> {
        let root_type =
            unsafe { sys::plasmite_lite3_get_root_type(self.bytes.as_ptr(), self.bytes.len()) };
        if root_type != sys::LITE3_TYPE_OBJECT {
            return Err(Error::new(ErrorKind::Corrupt).with_message("root is not object"));
        }

        let meta_type = unsafe {
            sys::plasmite_lite3_get_type(self.bytes.as_ptr(), self.bytes.len(), 0, c"meta".as_ptr())
        };
        if meta_type != sys::LITE3_TYPE_OBJECT {
            return Err(Error::new(ErrorKind::Corrupt).with_message("meta is not object"));
        }

        let meta_ofs = match get_key_offset_cstr_at(self.bytes, 0, c"meta") {
            Ok(ofs) => ofs,
            Err(err) => return Err(err.with_message("missing meta")),
        };

        let tags_type = unsafe {
            sys::plasmite_lite3_get_type(
                self.bytes.as_ptr(),
                self.bytes.len(),
                meta_ofs,
                c"tags".as_ptr(),
            )
        };
        if tags_type != sys::LITE3_TYPE_ARRAY {
            return Err(Error::new(ErrorKind::Corrupt).with_message("meta.tags must be array"));
        }

        let tags_ofs = get_key_offset_cstr_at(self.bytes, meta_ofs, c"tags")
            .map_err(|err| err.with_message("missing meta.tags"))?;

        let count = array_count(self.bytes, tags_ofs)?;
        for index in 0..count {
            self.array_str_at(tags_ofs, index)
                .map_err(|err| err.with_message("meta.tags must be string array"))?;
        }

        // Outer fields can be valid while nested data cannot produce a readable
        // message. Check that data before any append can commit its bytes.
        self.message_data_offset(data_kind)
    }

    pub(crate) fn message_data_offset(&self, data_kind: ErrorKind) -> Result<usize, Error> {
        let offset = self
            .key_offset_cstr_at(0, c"data")
            .map_err(|err| err.with_message("missing data"))?;
        match self.bytes.get(offset) {
            Some(&sys::LITE3_TYPE_OBJECT) => Ok(offset),
            Some(&kind) if kind < sys::LITE3_TYPE_INVALID => {
                Err(Error::new(data_kind).with_message("data is not object"))
            }
            _ => Err(Error::new(ErrorKind::Corrupt).with_message("invalid data")),
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        let data = self.validate_shape(ErrorKind::Corrupt)?;
        self.validate_readability_at(data)
    }

    /// Establish readability for an immutable view, including the whole envelope.
    pub(crate) fn validate_view(&self) -> Result<(), Error> {
        self.validate_shape(ErrorKind::Corrupt)?;
        match unsafe { sys::plasmite_lite3_verify_json(self.bytes.as_ptr(), self.bytes.len(), 0) } {
            0 | 1 => Ok(()),
            _ => Err(Error::new(ErrorKind::Corrupt).with_message("invalid lite3 message view")),
        }
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

pub(crate) fn validate_feed_bytes(buf: &[u8]) -> Result<(), Error> {
    let doc = Lite3DocRef::new(buf);
    let data = doc.validate_shape(ErrorKind::Usage)?;
    doc.validate_readability_at(data)
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
    get_key_offset_cstr_at(bytes, ofs, &c_key(key)?)
}

fn get_key_offset_cstr_at(bytes: &[u8], ofs: usize, key: &CStr) -> Result<usize, Error> {
    let mut out_ofs: usize = 0;
    let ret = unsafe {
        sys::plasmite_lite3_get_val_ofs(
            bytes.as_ptr(),
            bytes.len(),
            ofs,
            key.as_ptr(),
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
    fn encoding_parses_once_and_matches_the_pinned_decoder() {
        let fixtures = [
            json!({"x": 1}),
            json!((1000..2000).collect::<Vec<_>>()),
            json!(
                (0..100)
                    .map(|i| (format!("key{i}"), json!({"nested": [i, true, "α"]})))
                    .collect::<serde_json::Map<_, _>>()
            ),
            json!({"text": "x".repeat(40000)}),
            json!({"empty": "", "zero": "a\u{0000}b", "large": u64::MAX, "float": -0.0}),
            json!(null),
            json!(42),
            json!("scalar"),
        ];
        for fixture in fixtures {
            let json = serde_json::to_string(&fixture).unwrap();
            super::ENCODE_STATS.with(|stats| stats.set((0, 0, 0)));
            let encoded = Lite3Buf::from_json_str(&json).unwrap();
            let (parses, attempts, requested) = super::ENCODE_STATS.with(|stats| stats.get());
            assert_eq!(parses, 1);
            assert!(attempts >= 1 && requested >= encoded.len());
            if fixture.is_array() {
                assert!(attempts >= 2);
            }
            let mut legacy = vec![0; encoded.len().max(256)];
            let mut len = 0;
            assert_eq!(
                unsafe {
                    super::sys::plasmite_lite3_json_dec(
                        json.as_ptr().cast(),
                        json.len(),
                        legacy.as_mut_ptr(),
                        &mut len,
                        legacy.len(),
                    )
                },
                0
            );
            legacy.truncate(len);
            assert_eq!(encoded.as_slice(), legacy);
        }
        let oversized = format!(r#"{{"x":"{}"}}"#, "a".repeat(65536));
        assert!(Lite3Buf::from_json_str_with_limit(&oversized, 65536).is_err());
        // A large operation retains no scratch allocation for the next one.
        super::ENCODE_STATS.with(|stats| stats.set((0, 0, 0)));
        for _ in 0..100 {
            Lite3Buf::from_json_str("{}").unwrap();
        }
        assert_eq!(
            super::ENCODE_STATS.with(|stats| stats.get()),
            (100, 100, 25600)
        );
    }

    #[test]
    fn parsed_input_retains_exact_receipt_and_builds_one_message() {
        let input = br#"{"a":1,"a":2,"zero":-0,"text":"a\u0000b","wide":18446744073709551615}"#;
        super::ENCODE_STATS.with(|stats| stats.set((0, 0, 0)));
        let parsed = super::ParsedJson::parse(input).unwrap();
        let receipt = serde_json::to_value(parsed.value()).unwrap();
        assert_eq!(receipt["a"], 2);
        assert_eq!(receipt["wide"].as_u64(), Some(u64::MAX));
        assert_eq!(
            receipt["zero"].as_f64().unwrap().to_bits(),
            (-0.0_f64).to_bits()
        );
        let encoded = parsed.encode_message(&["sensor", "α"]).unwrap();
        encoded.as_doc().validate_view().unwrap();
        let stored: serde_json::Value =
            serde_json::from_str(&encoded.as_doc().to_json(false).unwrap()).unwrap();
        assert_eq!(stored["data"]["a"], 2);
        assert_eq!(stored["meta"]["tags"], json!(["sensor", "α"]));
        assert_eq!(stored["data"]["text"], "a\u{0000}b");
        assert_eq!(super::ENCODE_STATS.with(|stats| stats.get().0), 1);
        assert!(super::ParsedJson::parse(br#"{"bad\u0000key":1}"#).is_err());
    }

    #[test]
    fn view_encoder_grows_and_enforces_exact_integer_policy() {
        let text = format!(
            r#"{{"items":[{}],"zero":-0,"text":"α\u0000b"}}"#,
            (0..2000)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        let parsed = super::ParsedJson::parse(text.as_bytes()).unwrap();
        let view = parsed.encode_message_view(&["sensor"]).unwrap();
        assert!(view.bytes().len() > 256);
        let data: serde_json::Value =
            serde_json::from_slice(&view.data().to_json().unwrap()).unwrap();
        assert_eq!(data["items"][1999], 1999);
        assert_eq!(
            data["zero"].as_f64().unwrap().to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(data["text"], "α\u{0000}b");
        for number in [
            "9223372036854775808",
            "18446744073709551615",
            "18446744073709551616",
            "-9223372036854775809",
        ] {
            let text = format!("{{\"x\":{number}}}");
            assert!(
                super::ParsedJson::parse_view(text.as_bytes())
                    .and_then(|parsed| parsed.encode_message_view(&[]))
                    .is_err()
            );
        }
        let exact = super::ParsedJson::parse(br#"{"x":9223372036854775807,"x":4}"#).unwrap();
        let view = exact.encode_message_view(&[]).unwrap();
        assert_eq!(view.data().entries().unwrap().len(), 1);
        assert_eq!(view.data().get("x").unwrap().unwrap().integer().unwrap(), 4);
    }

    #[test]
    fn independent_encoders_share_no_document_or_scratch() {
        let workers = (0..4)
            .map(|id| {
                std::thread::spawn(move || {
                    for n in 0..100 {
                        let value =
                            json!({"id": id, "n": n, "values": (0..100).collect::<Vec<_>>()});
                        let json = serde_json::to_string(&value).unwrap();
                        let encoded = Lite3Buf::from_json_str(&json).unwrap();
                        let decoded: serde_json::Value =
                            serde_json::from_str(&encoded.as_doc().to_json(false).unwrap())
                                .unwrap();
                        assert_eq!(value, decoded);
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn encoding_tries_the_exact_limit_before_rejecting() {
        let limit = 64 * 1024;
        let overestimated = format!(r#"{{"x":"{}"}}"#, "a".repeat(40_000));
        assert!(overestimated.len() * 2 > limit);
        assert!(Lite3Buf::from_json_str_with_limit(&overestimated, limit).is_ok());
        let expanded = serde_json::to_string(&(1000..2000).collect::<Vec<_>>()).unwrap();
        let encoded = Lite3Buf::from_json_str_with_limit(&expanded, limit).unwrap();
        assert!(encoded.len() > expanded.len() * 2 * 4);
        let oversized = format!(r#"{{"x":"{}"}}"#, "a".repeat(limit));
        assert!(
            Lite3Buf::from_json_str_with_limit(&oversized, limit)
                .unwrap_err()
                .message()
                .unwrap()
                .contains("exceeded max")
        );
        assert!(
            Lite3Buf::from_json_str_with_limit("{", limit)
                .unwrap_err()
                .message()
                .unwrap()
                .contains("failed to encode")
        );
    }

    #[test]
    fn encoding_borrows_only_the_supplied_json_slice() {
        let storage = r#"{"x":1}not JSON"#;
        let encoded = Lite3Buf::from_json_str(&storage[..7]).expect("bounded input");
        assert_eq!(encoded.as_doc().to_json(false).unwrap(), r#"{"x":1}"#);
        let error = Lite3Buf::from_json_str("{\"x\":1}\0").unwrap_err();
        assert_eq!(error.message(), Some("json contains null"));
    }

    #[test]
    fn binary_readability_matches_export_for_utf8_keys_and_values() {
        let buf = encode_message(&[], &json!({"key_utf8_probe": "value_utf8_probe"})).unwrap();
        let data = buf.as_doc().key_offset("data").unwrap();
        for marker in [b"key_utf8_probe".as_slice(), b"value_utf8_probe".as_slice()] {
            let start = buf
                .as_slice()
                .windows(marker.len())
                .position(|s| s == marker)
                .unwrap();
            for pair in 0..=u16::MAX {
                let mut bytes = buf.as_slice().to_vec();
                bytes[start..start + 2].copy_from_slice(&pair.to_ne_bytes());
                let doc = Lite3DocRef::new(&bytes);
                let exported = doc
                    .to_json_at(data, false)
                    .map(|s| serde_json::from_str::<serde::de::IgnoredAny>(&s).is_ok())
                    .unwrap_or(false);
                assert_eq!(
                    doc.validate_readability_at(data).is_ok(),
                    exported,
                    "{marker:?}: {pair:x}"
                );
            }
            for prefix in [
                &[0xe0, 0xa0, 0x80][..],
                &[0xed, 0x9f, 0xbf],
                &[0xed, 0xa0, 0x80],
                &[0xf0, 0x90, 0x80, 0x80],
                &[0xf0, 0x8f, 0xbf, 0xbf],
                &[0xf4, 0x8f, 0xbf, 0xbf],
                &[0xf4, 0x90, 0x80, 0x80],
                &[0xe2, 0x82, 0xac],
                &[0xf0, 0x9f, 0x98, 0x80],
            ] {
                let mut bytes = buf.as_slice().to_vec();
                bytes[start..start + prefix.len()].copy_from_slice(prefix);
                let doc = Lite3DocRef::new(&bytes);
                let exported = doc.to_json_at(data, false).is_ok();
                assert_eq!(
                    doc.validate_readability_at(data).is_ok(),
                    exported,
                    "{prefix:x?}"
                );
            }
        }
    }

    #[test]
    fn long_ascii_runs_still_validate_multibyte_boundaries() {
        for offset in [7, 8, 31, 32, 39, 40, 63, 64, 95, 96] {
            let text = format!("{}🜁{}", "x".repeat(offset), "y".repeat(128 - offset));
            let encoded = encode_message(&[], &json!({"text": text})).unwrap();
            let mut bytes = encoded.as_slice().to_vec();
            let data = Lite3DocRef::new(&bytes).key_offset("data").unwrap();
            assert!(
                Lite3DocRef::new(&bytes)
                    .validate_readability_at(data)
                    .is_ok()
            );
            let marker = "🜁".as_bytes();
            let start = bytes
                .windows(marker.len())
                .position(|p| p == marker)
                .unwrap();
            bytes[start + 1] = 0xff;
            assert!(
                Lite3DocRef::new(&bytes)
                    .validate_readability_at(data)
                    .is_err()
            );
        }
    }

    #[test]
    fn json_round_trips_stored_float_bits() {
        let mut bits = 541u64;
        for _ in 0..10000 {
            bits ^= bits << 13;
            bits ^= bits >> 7;
            bits ^= bits << 17;
            let value = f64::from_bits(bits);
            if !value.is_finite() {
                continue;
            }
            let payload = encode_message(&[], &json!({"number":value})).unwrap();
            let doc = payload.as_doc();
            let data = doc.key_offset("data").unwrap();
            let exported = doc.to_json_at(data, false).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&exported).unwrap();
            assert_eq!(
                parsed["number"].as_f64().unwrap().to_bits(),
                value.to_bits(),
                "{exported}"
            );
        }
    }

    #[test]
    fn native_bytes_keep_the_legacy_export_validation() {
        let buf = encode_message(&[], &json!({"blob": "bytes including zero"})).unwrap();
        let data = buf.as_doc().key_offset("data").unwrap();
        let value = buf.as_doc().key_offset_at(data, "blob").unwrap();
        let mut bytes = buf.as_slice().to_vec();
        // String and byte values share their length-prefixed storage. Treat the
        // terminator as one more byte, which the legacy exporter base64-encodes.
        bytes[value] = super::sys::LITE3_TYPE_BYTES;
        let doc = Lite3DocRef::new(&bytes);
        assert_eq!(
            unsafe { super::sys::plasmite_lite3_verify_json(bytes.as_ptr(), bytes.len(), data) },
            1
        );
        assert!(doc.to_json_at(data, false).is_ok());
        assert!(doc.validate_readability_at(data).is_ok());
    }

    #[test]
    fn binary_readability_matches_export_for_float_extremes() {
        let number = f64::from_bits(0x3ff1_2345_6789_abcd);
        let buf = encode_message(&[], &json!({"number": number})).unwrap();
        let data = buf.as_doc().key_offset("data").unwrap();
        let start = buf
            .as_slice()
            .windows(8)
            .position(|s| s == number.to_ne_bytes())
            .unwrap();
        for value in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            f64::MIN,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            -0.0,
        ] {
            let mut bytes = buf.as_slice().to_vec();
            bytes[start..start + 8].copy_from_slice(&value.to_ne_bytes());
            let doc = Lite3DocRef::new(&bytes);
            assert_eq!(
                doc.validate_readability_at(data).is_ok(),
                doc.to_json_at(data, false).is_ok(),
                "{value}"
            );
        }
    }

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
    fn wide_nested_objects_preserve_every_value() {
        // Seven slots fill one node; larger objects exercise child traversal,
        // iterator ascent, and the prefetch at the end of each node.
        for count in [7, 8, 100, 1000] {
            let fields: serde_json::Map<_, _> =
                (0..count).map(|i| (format!("key{i}"), json!(i))).collect();
            let data = json!({"nested": [{"fields": fields}]});
            let buf = encode_message(&["event".into()], &data).expect("encode");
            let doc = buf.as_doc();
            doc.validate().expect("valid wide message");
            let data_ofs = doc.key_offset("data").expect("data");
            let nested = doc.key_offset_at(data_ofs, "nested").expect("nested");
            assert_eq!(doc.count_at(nested).expect("array count"), 1);
            for pretty in [false, true] {
                let decoded: serde_json::Value =
                    serde_json::from_str(&doc.to_json(pretty).expect("decode")).expect("JSON");
                assert_eq!(decoded, json!({"meta": {"tags": ["event"]}, "data": data}));
            }
            // Lookup must also traverse the B-tree, independently of iteration.
            let direct = Lite3Buf::from_json_str(&serde_json::to_string(&fields).unwrap())
                .expect("encode fields");
            for i in 0..count {
                assert_eq!(
                    direct.as_doc().i64_at_key(0, &format!("key{i}")).unwrap(),
                    i
                );
            }
        }
    }

    #[test]
    fn malformed_internal_children_are_rejected() {
        let fields: serde_json::Map<_, _> =
            (0..1000).map(|i| (format!("key{i}"), json!(i))).collect();
        let buf = encode_message(&[], &json!({"nested": fields})).expect("encode");
        let data = buf.as_doc().key_offset("data").unwrap();
        let nested = buf.as_doc().key_offset_at(data, "nested").unwrap();
        // Lite3 nodes have eight u32 child offsets starting at byte 64.
        let first_child =
            u32::from_le_bytes(buf.as_slice()[nested + 64..nested + 68].try_into().unwrap())
                as usize;
        assert_ne!(
            first_child, 0,
            "fixture must contain multiple B-tree levels"
        );
        for (parent, edge) in [(data, 0), (nested, 0), (nested, 1), (first_child, 0)] {
            for bad_offset in [1u32, 2, 3, u32::MAX - 3] {
                let mut bytes = buf.as_slice().to_vec();
                let child = parent + 64 + edge * 4;
                bytes[child..child + 4].copy_from_slice(&bad_offset.to_le_bytes());
                let doc = Lite3DocRef::new(&bytes);
                for pretty in [false, true] {
                    assert!(
                        doc.to_json_at(data, pretty).is_err(),
                        "parent={parent}, offset={bad_offset}"
                    );
                }
                assert!(validate_bytes(&bytes).is_err());
            }
        }
        // Force lookup down a bad edge, regardless of the key's hash position.
        let mut bytes = buf.as_slice().to_vec();
        for child in bytes[nested + 64..nested + 96].as_chunks_mut::<4>().0 {
            child.copy_from_slice(&1u32.to_le_bytes());
        }
        assert!(
            Lite3DocRef::new(&bytes)
                .i64_at_key(nested, "missing")
                .is_err()
        );
    }

    #[test]
    fn malformed_child_append_leaves_pool_unchanged() {
        use crate::api::{AppendOptions, Durability, ErrorKind, Pool, PoolApiExt, PoolOptions};

        let payload = encode_message(&[], &json!({"x": true})).expect("encode");
        let data = payload.as_doc().key_offset("data").unwrap();
        let mut bytes = payload.as_slice().to_vec();
        bytes[data + 64..data + 68].copy_from_slice(&1u32.to_le_bytes());
        let temp = tempfile::tempdir().unwrap();
        let mut pool = Pool::create(
            temp.path().join("children.plasmite"),
            PoolOptions::new(1024 * 1024),
        )
        .unwrap();
        let before = pool.info().unwrap().bounds;
        let options = AppendOptions::new(123, Durability::Fast);
        assert_eq!(
            pool.append_lite3(&bytes, options).unwrap_err().kind(),
            ErrorKind::Corrupt
        );
        assert_eq!(pool.info().unwrap().bounds, before);
        let appended = pool
            .append_lite3(payload.as_slice(), options)
            .expect("valid append after rejection");
        assert_eq!(appended, 1);
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
        bytes[tags + 32..tags + 36].copy_from_slice(&(!63u32).to_le_bytes());
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
