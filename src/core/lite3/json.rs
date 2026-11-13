//! Serialize a validated immutable Lite3 view without building an owned data tree.
use super::{Lite3DocRef, sys};
use crate::core::error::{Error, ErrorKind};
use serde::ser::{Error as _, Serialize, SerializeMap, SerializeSeq, Serializer};

pub(crate) struct JsonValue<'a> {
    doc: Lite3DocRef<'a>,
    offset: usize,
}

impl<'a> JsonValue<'a> {
    /// Borrow immutable, fully checked bytes through every derived child.
    pub(crate) fn new(doc: Lite3DocRef<'a>, offset: usize) -> Result<Self, Error> {
        match unsafe {
            sys::plasmite_lite3_verify_json(doc.bytes().as_ptr(), doc.bytes().len(), offset)
        } {
            0 | 1 => Ok(Self { doc, offset }),
            _ => Err(Error::new(ErrorKind::Corrupt).with_message("failed to decode lite3")),
        }
    }
}

impl Serialize for JsonValue<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let bytes = self.doc.bytes();
        let mut value = sys::ValueView::default();
        // SAFETY: Construction verifies this immutable tree. Child offsets and
        // borrowed strings come only from that tree's checked native iterator.
        unsafe { sys::plasmite_lite3_value_view(bytes.as_ptr(), self.offset, &mut value) };
        match value.kind {
            sys::LITE3_TYPE_NULL => serializer.serialize_unit(),
            sys::LITE3_TYPE_BOOL => serializer.serialize_bool(value.integer != 0),
            sys::LITE3_TYPE_I64 => serializer.serialize_i64(value.integer),
            sys::LITE3_TYPE_F64 => serializer.serialize_f64(value.real),
            sys::LITE3_TYPE_BYTES => {
                let bytes =
                    unsafe { std::slice::from_raw_parts(value.text.cast::<u8>(), value.text_len) };
                serializer.collect_str(&base64::display::Base64Display::new(
                    bytes,
                    &base64::engine::general_purpose::STANDARD,
                ))
            }
            sys::LITE3_TYPE_STRING => {
                let text =
                    unsafe { std::slice::from_raw_parts(value.text.cast::<u8>(), value.text_len) };
                // Construction checked UTF-8 over these immutable bytes once.
                serializer.serialize_str(unsafe { std::str::from_utf8_unchecked(text) })
            }
            sys::LITE3_TYPE_OBJECT | sys::LITE3_TYPE_ARRAY => {
                let count = self.doc.count_at(self.offset).map_err(S::Error::custom)? as usize;
                if count > bytes.len() {
                    return Err(S::Error::custom("invalid collection count"));
                }
                let mut entries = vec![sys::Entry::default(); count];
                let ret = unsafe {
                    sys::plasmite_lite3_entries(
                        bytes.as_ptr(),
                        bytes.len(),
                        self.offset,
                        entries.as_mut_ptr(),
                        count,
                    )
                };
                if ret < 0 {
                    return Err(S::Error::custom("invalid collection"));
                }
                let child = |offset| Self {
                    doc: self.doc,
                    offset,
                };
                if value.kind == sys::LITE3_TYPE_ARRAY {
                    let mut array = serializer.serialize_seq(Some(count))?;
                    for entry in entries {
                        array.serialize_element(&child(entry.offset))?;
                    }
                    array.end()
                } else {
                    let key = |entry: &sys::Entry| {
                        let bytes = unsafe {
                            std::slice::from_raw_parts(entry.key.cast::<u8>(), entry.key_len)
                        };
                        // Full validation checked UTF-8 and the terminated key.
                        unsafe { std::str::from_utf8_unchecked(bytes) }
                    };
                    // Stable sort retains native iteration order for duplicate
                    // keys; serde's Value parser keeps the last occurrence.
                    entries.sort_by(|a, b| key(a).cmp(key(b)));
                    let mut object = serializer.serialize_map(None)?;
                    for (index, entry) in entries.iter().enumerate() {
                        if entries
                            .get(index + 1)
                            .is_some_and(|next| key(entry) == key(next))
                        {
                            continue;
                        }
                        object.serialize_entry(key(entry), &child(entry.offset))?;
                    }
                    object.end()
                }
            }
            _ => Err(S::Error::custom("invalid JSON value")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::JsonValue;
    use crate::core::lite3::{Lite3Buf, Lite3DocRef, sys};

    fn compare(input: &str) {
        let payload = Lite3Buf::from_json_str(input).unwrap();
        let doc = payload.as_doc();
        let export = doc.to_json(false).unwrap();
        let legacy: serde_json::Value = serde_json::from_str(&export).unwrap();
        let view = JsonValue::new(doc, 0).unwrap();
        assert_eq!(
            serde_json::to_vec(&view).unwrap(),
            serde_json::to_vec(&legacy).unwrap(),
            "{input}"
        );
    }

    #[test]
    fn borrowed_serialization_matches_correctly_rounded_json() {
        for input in [
            r#"{"z":1,"a":2,"nested":{"b":1,"a":2}}"#,
            r#"{"x":-0.0,"e":1e20,"tiny":1e-20}"#,
            r#"{"text":"line\n\u00e9\ud83e\uddea\/\u0000","array":[true,false,null,"",{},[]]}"#,
            r#"{"x":9223372036854775807,"u":18446744073709551615,"min":-9223372036854775808}"#,
            r#"{"x":1.7976931348623157e308,"tiny":5e-324}"#,
            r#"{"x":2.027716612057172e155}"#,
            r#"{"x":-9.06020532968062e105}"#,
            r#"{"x":-2.2287787288054198e-210}"#,
        ] {
            compare(input);
        }
        let mut bits = 541u64;
        for _ in 0..5000 {
            bits ^= bits << 13;
            bits ^= bits >> 7;
            bits ^= bits << 17;
            let value = f64::from_bits(bits);
            if value.is_finite() {
                compare(&format!("{{\"x\":{value:?}}}"));
            }
        }
    }

    #[test]
    fn native_bytes_stream_the_same_base64_without_a_json_round_trip() {
        for len in 0..512 {
            let payload =
                Lite3Buf::from_json_str(&serde_json::json!({"blob":"x".repeat(len)}).to_string())
                    .unwrap();
            let offset = payload.as_doc().key_offset("blob").unwrap();
            let mut value = sys::ValueView::default();
            unsafe {
                sys::plasmite_lite3_value_view(payload.as_slice().as_ptr(), offset, &mut value)
            };
            let start = value.text as usize - payload.as_slice().as_ptr() as usize;
            let mut bytes = payload.as_slice().to_vec();
            // Strings and byte values use the same length prefix. The original
            // terminator becomes another byte, matching existing native behavior.
            bytes[offset] = sys::LITE3_TYPE_BYTES;
            for (index, byte) in bytes[start..start + len].iter_mut().enumerate() {
                *byte = index.wrapping_mul(17) as u8;
            }
            let doc = Lite3DocRef::new(&bytes);
            let legacy: serde_json::Value =
                serde_json::from_str(&doc.to_json(false).unwrap()).unwrap();
            let direct = serde_json::to_vec(&JsonValue::new(doc, 0).unwrap()).unwrap();
            assert_eq!(direct, serde_json::to_vec(&legacy).unwrap(), "length {len}");
        }
    }
}
