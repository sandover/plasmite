//! Format views directly, retaining key order when building native containers.
use super::{MessageView, ValueRef, corrupt};
use crate::core::error::Error;
use crate::core::lite3::{JsonValue, sys};
use serde::ser::{Error as _, Serialize, SerializeMap, SerializeSeq, Serializer};

struct Ordered<'a>(ValueRef<'a>);
impl Serialize for Ordered<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = self.0;
        let node = value.owner.node(value.offset);
        match node.kind {
            sys::LITE3_TYPE_NULL => serializer.serialize_unit(),
            sys::LITE3_TYPE_BOOL => serializer.serialize_bool(node.integer != 0),
            sys::LITE3_TYPE_I64 => serializer.serialize_i64(node.integer),
            sys::LITE3_TYPE_F64 => serializer.serialize_f64(node.real),
            sys::LITE3_TYPE_STRING => {
                // The immutable owner checked text before publishing this node.
                let bytes = unsafe { std::slice::from_raw_parts(node.text.cast(), node.text_len) };
                serializer.serialize_str(unsafe { std::str::from_utf8_unchecked(bytes) })
            }
            sys::LITE3_TYPE_BYTES => {
                let bytes = unsafe { std::slice::from_raw_parts(node.text.cast(), node.text_len) };
                serializer.collect_str(&base64::display::Base64Display::new(
                    bytes,
                    &base64::engine::general_purpose::STANDARD,
                ))
            }
            sys::LITE3_TYPE_OBJECT => {
                let mut map =
                    serializer.serialize_map(Some(value.len().map_err(S::Error::custom)?))?;
                for entry in value.iter().map_err(S::Error::custom)? {
                    let (key, child) = entry.map_err(S::Error::custom)?;
                    map.serialize_entry(
                        key.ok_or_else(|| S::Error::custom("missing object key"))?,
                        &Ordered(child),
                    )?;
                }
                map.end()
            }
            sys::LITE3_TYPE_ARRAY => {
                let mut array =
                    serializer.serialize_seq(Some(value.len().map_err(S::Error::custom)?))?;
                for entry in value.iter().map_err(S::Error::custom)? {
                    let (_, child) = entry.map_err(S::Error::custom)?;
                    array.serialize_element(&Ordered(child))?;
                }
                array.end()
            }
            _ => Err(S::Error::custom("unsupported view value")),
        }
    }
}
impl ValueRef<'_> {
    /// Internal native-copy route; ordinary JSON export keeps canonical key order.
    pub fn to_json_stored(self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(&Ordered(self)).map_err(|_| corrupt())
    }
}
impl MessageView {
    /// Format the complete envelope directly from its immutable data.
    pub fn to_json(&self) -> Result<Vec<u8>, Error> {
        let tags = self
            .root()
            .get("meta")?
            .ok_or_else(corrupt)?
            .get("tags")?
            .ok_or_else(corrupt)?;
        let tags = tags
            .iter()?
            .map(|entry| entry?.1.text().ok_or_else(corrupt))
            .collect::<Result<Vec<_>, _>>()?;
        let data = unsafe { JsonValue::from_validated(self.doc(), self.data().offset) };
        let wire = crate::interface_wire::MessageWire::new(
            self.seq,
            crate::api::format_ts(self.timestamp_ns)?,
            &tags,
            data,
        );
        serde_json::to_vec(&wire).map_err(|_| corrupt())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lite3::Lite3Buf;
    #[test]
    fn stored_order_preserves_values_and_native_iteration() {
        let encoded = Lite3Buf::from_json_str(r#"{"meta":{"tags":[]},"data":{"z":1,"a":2,"__proto__":7,"zero":-0.0,"text":"a\u0000b","large":9223372036854775807}}"#).unwrap();
        let view = MessageView::from_bytes(encoded.as_slice(), u64::MAX, 1).unwrap();
        let data = view.data();
        let bytes = data.to_json_stored().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        let keys = data
            .iter()
            .unwrap()
            .map(|entry| entry.unwrap().0.unwrap())
            .collect::<Vec<_>>();
        let positions = keys
            .iter()
            .map(|key| {
                text.find(&format!("{}:", serde_json::to_string(key).unwrap()))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(positions.windows(2).all(|p| p[0] < p[1]));
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value["zero"].as_f64().unwrap().to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(value["large"].as_i64(), Some(i64::MAX));
        assert_eq!(value["text"], "a\0b");
        let full: serde_json::Value = serde_json::from_slice(&view.to_json().unwrap()).unwrap();
        assert_eq!(full["seq"].as_u64(), Some(u64::MAX));
        assert_eq!(full["time"], "1970-01-01T00:00:00.000000001Z");
        assert_eq!(full["data"], value);
    }
}
