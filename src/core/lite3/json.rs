//! Convert validated Lite3 and parsed JSON values to JSON text or an owned tree.
use super::{Lite3DocRef, ParsedJson, sys};
use crate::core::error::{Error, ErrorKind};
use base64::Engine as _;
use serde::ser::{Error as _, Serialize, SerializeMap, SerializeSeq, Serializer};
use serde_json::ser::{CharEscape, CompactFormatter, Formatter};
use serde_json::{Map, Number, Value};

#[derive(Clone, Copy)]
enum Source<'a> {
    Lite3(Lite3DocRef<'a>),
    Parsed(&'a ParsedJson),
}

pub(crate) struct JsonValue<'a> {
    source: Source<'a>,
    offset: usize,
}

impl<'a> JsonValue<'a> {
    /// Borrow immutable, fully checked bytes through every derived child.
    pub(crate) fn new(doc: Lite3DocRef<'a>, offset: usize) -> Result<Self, Error> {
        match unsafe {
            sys::plasmite_lite3_verify_json(doc.bytes().as_ptr(), doc.bytes().len(), offset)
        } {
            // The checked walk established readability of this immutable subtree.
            0 | 1 => Ok(unsafe { Self::from_validated(doc, offset) }),
            _ => Err(Error::new(ErrorKind::Corrupt).with_message("failed to decode lite3")),
        }
    }
    pub(crate) fn for_stream(doc: Lite3DocRef<'a>, offset: usize) -> Result<Self, Error> {
        match unsafe {
            sys::plasmite_lite3_verify_json(doc.bytes().as_ptr(), doc.bytes().len(), offset)
        } {
            0 => {}
            // The raw-stream path also calls the exporter on native bytes.
            1 => {
                doc.to_json_at(offset, false)?;
            }
            _ => return Err(corrupt("failed to decode lite3")),
        }
        Ok(unsafe { Self::from_validated(doc, offset) })
    }

    /// Format a node whose immutable owner established whole-frame readability.
    ///
    /// # Safety
    /// The node and descendants must be readable, and the bytes must retain that
    /// validation proof and immutable lifetime.
    pub(crate) unsafe fn from_validated(doc: Lite3DocRef<'a>, offset: usize) -> Self {
        Self {
            source: Source::Lite3(doc),
            offset,
        }
    }
    pub(super) fn from_parsed(doc: &'a ParsedJson) -> Self {
        Self {
            source: Source::Parsed(doc),
            offset: unsafe { sys::plasmite_json_root(doc.ptr.as_ptr()) },
        }
    }
    /// Check the subtree and its counts before using them to allocate an owned value.
    pub(crate) fn decode(doc: Lite3DocRef<'a>, offset: usize) -> Result<Value, Error> {
        if unsafe {
            sys::plasmite_lite3_verify_json_counts(doc.bytes().as_ptr(), doc.bytes().len(), offset)
        } < 0
        {
            return Err(corrupt("failed to decode lite3"));
        }
        unsafe { Self::from_validated(doc, offset) }.to_owned_value()
    }

    fn to_owned_value(&self) -> Result<Value, Error> {
        let Source::Lite3(doc) = self.source else {
            unreachable!("direct decoding starts from Lite3");
        };
        let bytes = &doc.bytes()[self.offset..];
        let kind = bytes[0];
        // The checked walk guarantees the scalar span. Lite3 stores a
        // one-byte type followed by the native-endian scalar payload.
        Ok(match kind {
            sys::LITE3_TYPE_NULL => Value::Null,
            sys::LITE3_TYPE_BOOL => match bytes[1] {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                _ => return Err(corrupt("invalid JSON boolean")),
            },
            sys::LITE3_TYPE_I64 => value_i64(bytes),
            sys::LITE3_TYPE_F64 => Value::Number(
                Number::from_f64(f64::from_ne_bytes(number_bytes(bytes)))
                    .ok_or_else(|| corrupt("invalid JSON number"))?,
            ),
            sys::LITE3_TYPE_STRING => {
                let value = self.view();
                let bytes =
                    unsafe { std::slice::from_raw_parts(value.text.cast::<u8>(), value.text_len) };
                // The validation pass checked these immutable bytes.
                Value::String(unsafe { std::str::from_utf8_unchecked(bytes) }.to_owned())
            }
            sys::LITE3_TYPE_BYTES => {
                let value = self.view();
                let bytes =
                    unsafe { std::slice::from_raw_parts(value.text.cast::<u8>(), value.text_len) };
                Value::String(base64::engine::general_purpose::STANDARD.encode(bytes))
            }
            sys::LITE3_TYPE_ARRAY => {
                let entries = self.entries()?;
                let mut array = Vec::with_capacity(entries.len());
                for entry in entries {
                    let bytes = &doc.bytes()[entry.offset..];
                    let child = if bytes[0] == sys::LITE3_TYPE_I64 {
                        value_i64(bytes)
                    } else {
                        self.child(entry.offset).to_owned_value()?
                    };
                    array.push(child);
                }
                Value::Array(array)
            }
            sys::LITE3_TYPE_OBJECT => {
                let mut object = Map::new();
                for entry in self.entries()? {
                    // Insertion in stored order gives duplicate names the same
                    // last-wins behavior as parsing the exported JSON text.
                    object.insert(
                        entry_key(&entry).to_owned(),
                        self.child(entry.offset).to_owned_value()?,
                    );
                }
                Value::Object(object)
            }
            _ => return Err(corrupt("invalid JSON value")),
        })
    }

    fn child(&self, offset: usize) -> Self {
        Self {
            source: self.source,
            offset,
        }
    }

    fn entries(&self) -> Result<Vec<sys::Entry>, Error> {
        let count = match self.source {
            Source::Lite3(doc) => doc.count_at(self.offset)? as usize,
            Source::Parsed(_) => unsafe { sys::plasmite_json_count(self.offset) },
        };
        let bound = match self.source {
            Source::Lite3(doc) => doc.bytes().len(),
            Source::Parsed(doc) => doc.values,
        };
        if count > bound {
            return Err(corrupt("invalid collection count"));
        }
        let mut entries = vec![sys::Entry::default(); count];
        let ret = unsafe {
            match self.source {
                Source::Lite3(doc) => sys::plasmite_lite3_entries(
                    doc.bytes().as_ptr(),
                    doc.bytes().len(),
                    self.offset,
                    entries.as_mut_ptr(),
                    count,
                ),
                Source::Parsed(_) => {
                    sys::plasmite_json_entries(self.offset, entries.as_mut_ptr(), count)
                }
            }
        };
        if ret < 0 {
            return Err(corrupt("invalid collection"));
        }
        Ok(entries)
    }

    fn view(&self) -> sys::ValueView {
        let mut value = sys::ValueView::default();
        unsafe {
            match self.source {
                Source::Lite3(doc) => {
                    sys::plasmite_lite3_value_view(doc.bytes().as_ptr(), self.offset, &mut value)
                }
                Source::Parsed(_) => sys::plasmite_json_value_view(self.offset, &mut value),
            }
        };
        value
    }
    pub(crate) fn write_json(&self, out: &mut Vec<u8>) -> Result<(), serde_json::Error> {
        let value = self.view();
        match value.kind {
            sys::LITE3_TYPE_STRING if value.text_len >= 1024 => {
                let bytes =
                    unsafe { std::slice::from_raw_parts(value.text.cast::<u8>(), value.text_len) };
                quote_long_string(out, bytes).map_err(serde_json::Error::io)
            }
            sys::LITE3_TYPE_ARRAY => {
                out.push(b'[');
                for (index, entry) in self
                    .entries()
                    .map_err(serde_json::Error::custom)?
                    .into_iter()
                    .enumerate()
                {
                    if index > 0 {
                        out.push(b',');
                    }
                    self.child(entry.offset).write_json(out)?;
                }
                out.push(b']');
                Ok(())
            }
            sys::LITE3_TYPE_OBJECT => {
                out.push(b'{');
                let mut entries = self.entries().map_err(serde_json::Error::custom)?;
                entries.sort_by(|a, b| entry_key(a).cmp(entry_key(b)));
                let mut first = true;
                for (index, entry) in entries.iter().enumerate() {
                    if entries
                        .get(index + 1)
                        .is_some_and(|next| entry_key(entry) == entry_key(next))
                    {
                        continue;
                    }
                    if !first {
                        out.push(b',');
                    }
                    first = false;
                    serde_json::to_writer(&mut *out, entry_key(entry))?;
                    out.push(b':');
                    self.child(entry.offset).write_json(out)?;
                }
                out.push(b'}');
                Ok(())
            }
            _ => serde_json::to_writer(out, self),
        }
    }

    pub(super) fn kind(&self) -> u8 {
        self.view().kind
    }
}

impl Serialize for JsonValue<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Both sources own/borrow a validated immutable tree for this call.
        let value = self.view();
        match value.kind {
            sys::LITE3_TYPE_NULL => serializer.serialize_unit(),
            sys::LITE3_TYPE_BOOL => serializer.serialize_bool(value.integer != 0),
            sys::LITE3_TYPE_I64 => serializer.serialize_i64(value.integer),
            sys::JSON_TYPE_U64 => serializer.serialize_u64(value.integer as u64),
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
                let mut entries = self.entries().map_err(S::Error::custom)?;
                let count = entries.len();
                if value.kind == sys::LITE3_TYPE_ARRAY {
                    let mut array = serializer.serialize_seq(Some(count))?;
                    for entry in entries {
                        array.serialize_element(&self.child(entry.offset))?;
                    }
                    array.end()
                } else {
                    // Stable sort retains native iteration order for duplicate
                    // keys; serde's Value parser keeps the last occurrence.
                    entries.sort_by(|a, b| entry_key(a).cmp(entry_key(b)));
                    let mut object = serializer.serialize_map(None)?;
                    for (index, entry) in entries.iter().enumerate() {
                        if entries
                            .get(index + 1)
                            .is_some_and(|next| entry_key(entry) == entry_key(next))
                        {
                            continue;
                        }
                        object.serialize_entry(entry_key(entry), &self.child(entry.offset))?;
                    }
                    object.end()
                }
            }
            _ => Err(S::Error::custom("invalid JSON value")),
        }
    }
}

// Find the first byte requiring a JSON escape. Word subtraction can mark an
// extra byte after a match, so only use the lowest marked byte in each word.
fn json_escape_offset(bytes: &[u8]) -> usize {
    const ONES: u64 = u64::MAX / 255;
    const HIGH: u64 = ONES << 7;
    let (chunks, tail) = bytes.as_chunks::<8>();
    for (index, chunk) in chunks.iter().enumerate() {
        let word = u64::from_le_bytes(*chunk);
        let quotes = word ^ (ONES * u64::from(b'"'));
        let slashes = word ^ (ONES * u64::from(b'\\'));
        let controls = word.wrapping_sub(ONES * 0x20) & !word;
        let matches = (controls
            | (quotes.wrapping_sub(ONES) & !quotes)
            | (slashes.wrapping_sub(ONES) & !slashes))
            & HIGH;
        if matches != 0 {
            return index * 8 + matches.trailing_zeros() as usize / 8;
        }
    }
    bytes.len() - tail.len()
        + tail
            .iter()
            .position(|&byte| byte < 0x20 || matches!(byte, b'"' | b'\\'))
            .unwrap_or(tail.len())
}

fn quote_long_string(quoted: &mut Vec<u8>, bytes: &[u8]) -> std::io::Result<()> {
    let mut formatter = CompactFormatter;
    formatter.begin_string(quoted)?;
    let mut bytes = bytes;
    loop {
        let end = json_escape_offset(bytes);
        quoted.extend_from_slice(&bytes[..end]);
        let Some((&byte, rest)) = bytes[end..].split_first() else {
            break;
        };
        let escape = match byte {
            b'"' => CharEscape::Quote,
            b'\\' => CharEscape::ReverseSolidus,
            b'\x08' => CharEscape::Backspace,
            b'\x0c' => CharEscape::FormFeed,
            b'\n' => CharEscape::LineFeed,
            b'\r' => CharEscape::CarriageReturn,
            b'\t' => CharEscape::Tab,
            ctrl => CharEscape::AsciiControl(ctrl),
        };
        formatter.write_char_escape(quoted, escape)?;
        bytes = rest;
    }
    formatter.end_string(quoted)
}

fn corrupt(message: &'static str) -> Error {
    Error::new(ErrorKind::Corrupt).with_message(message)
}

fn number_bytes(bytes: &[u8]) -> [u8; 8] {
    bytes[1..9].try_into().expect("checked Lite3 number")
}

fn value_i64(bytes: &[u8]) -> Value {
    Value::Number(i64::from_ne_bytes(number_bytes(bytes)).into())
}

fn entry_key(entry: &sys::Entry) -> &str {
    let bytes = unsafe { std::slice::from_raw_parts(entry.key.cast::<u8>(), entry.key_len) };
    // Both sources hold checked, immutable UTF-8 keys during traversal.
    unsafe { std::str::from_utf8_unchecked(bytes) }
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
