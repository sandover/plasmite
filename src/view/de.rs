//! Deserialize directly from immutable values, borrowing text and bytes.
use super::{Entries, ValueRef};
use crate::core::error::{Error, ErrorKind};
use crate::core::lite3::sys;
use serde::de::{
    self, Deserialize, DeserializeSeed, EnumAccess, IntoDeserializer, MapAccess, SeqAccess,
    VariantAccess, Visitor,
};

type DecodeError = de::value::Error;
fn invalid(error: impl std::fmt::Display) -> DecodeError {
    de::Error::custom(error)
}

impl<'de> ValueRef<'de> {
    /// Build the requested type directly; borrowed fields share this snapshot.
    /// Struct decoding scans fields. Use keyed access for an indexed projection.
    ///
    /// ```compile_fail
    /// use plasmite::view::MessageView;
    /// fn escape(owner: MessageView) -> &'static str {
    ///     owner.data().get("name").unwrap().unwrap().decode().unwrap()
    /// }
    /// ```
    pub fn decode<T: Deserialize<'de>>(self) -> Result<T, Error> {
        T::deserialize(self).map_err(|e| Error::new(ErrorKind::Usage).with_message(e.to_string()))
    }
}
impl<'de> IntoDeserializer<'de, DecodeError> for ValueRef<'de> {
    type Deserializer = Self;
    fn into_deserializer(self) -> Self {
        self
    }
}
// Map keys are stored as text, but Serde maps may request scalar keys.
struct Key<'de>(&'de str);
macro_rules! numeric_key {
    ($($method:ident),* $(,)?) => { $(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
            // The key already is text. Reuse JSON's numeric grammar without
            // constructing a JSON string or generic value.
            if self.0.bytes().any(|b| b.is_ascii_whitespace()) { return Err(invalid("invalid numeric map key")); }
            let mut parser = serde_json::Deserializer::from_str(self.0);
            let value = de::Deserializer::$method(&mut parser, visitor).map_err(invalid)?;
            parser.end().map_err(invalid)?;
            Ok(value)
        }
    )* };
}
impl<'de> de::Deserializer<'de> for Key<'de> {
    type Error = DecodeError;
    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        visitor.visit_borrowed_str(self.0)
    }
    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        visitor.visit_bool(self.0.parse::<bool>().map_err(invalid)?)
    }
    numeric_key!(
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_i128,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_u128,
        deserialize_f32,
        deserialize_f64
    );
    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DecodeError> {
        visitor.visit_some(self)
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, DecodeError> {
        visitor.visit_newtype_struct(self)
    }
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DecodeError> {
        visitor.visit_enum(de::value::BorrowedStrDeserializer::<DecodeError>::new(
            self.0,
        ))
    }
    serde::forward_to_deserialize_any! { char str string bytes byte_buf unit unit_struct seq tuple tuple_struct map struct identifier ignored_any }
}
struct Access<'de> {
    entries: Entries<'de>,
    pending: Option<ValueRef<'de>>,
    remaining: usize,
}
impl<'de> MapAccess<'de> for Access<'de> {
    type Error = DecodeError;
    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        let Some(entry) = self.entries.next() else {
            return Ok(None);
        };
        let (key, child) = entry.map_err(invalid)?;
        let key = key.ok_or_else(|| invalid("missing object key"))?;
        self.remaining -= 1;
        self.pending = Some(child);
        seed.deserialize(Key(key)).map(Some)
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.remaining)
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        seed.deserialize(
            self.pending
                .take()
                .ok_or_else(|| invalid("value requested before key"))?,
        )
    }
}
impl<'de> SeqAccess<'de> for Access<'de> {
    type Error = DecodeError;
    fn size_hint(&self) -> Option<usize> {
        Some(self.remaining)
    }
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        let Some(entry) = self.entries.next() else {
            return Ok(None);
        };
        self.remaining -= 1;
        seed.deserialize(entry.map_err(invalid)?.1).map(Some)
    }
}
struct Enum<'de> {
    key: &'de str,
    value: ValueRef<'de>,
}
impl<'de> EnumAccess<'de> for Enum<'de> {
    type Error = DecodeError;
    type Variant = ValueRef<'de>;
    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Self::Error> {
        Ok((
            seed.deserialize(de::value::BorrowedStrDeserializer::new(self.key))?,
            self.value,
        ))
    }
}
impl<'de> VariantAccess<'de> for ValueRef<'de> {
    type Error = DecodeError;
    fn unit_variant(self) -> Result<(), Self::Error> {
        Deserialize::deserialize(self)
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, Self::Error> {
        seed.deserialize(self)
    }
    fn tuple_variant<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        de::Deserializer::deserialize_tuple(self, len, visitor)
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        de::Deserializer::deserialize_struct(self, "", fields, visitor)
    }
}
impl<'de> de::Deserializer<'de> for ValueRef<'de> {
    type Error = DecodeError;
    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = self.owner.node(self.offset);
        match node.kind {
            sys::LITE3_TYPE_NULL => visitor.visit_unit(),
            sys::LITE3_TYPE_BOOL => visitor.visit_bool(node.integer != 0),
            sys::LITE3_TYPE_I64 => visitor.visit_i64(node.integer),
            sys::LITE3_TYPE_F64 => visitor.visit_f64(node.real),
            sys::LITE3_TYPE_STRING => {
                visitor.visit_borrowed_str(self.text().ok_or_else(|| invalid("invalid text"))?)
            }
            sys::LITE3_TYPE_BYTES => {
                visitor.visit_borrowed_bytes(self.span().ok_or_else(|| invalid("invalid bytes"))?)
            }
            sys::LITE3_TYPE_OBJECT => visitor.visit_map(Access {
                entries: self.iter().map_err(invalid)?,
                pending: None,
                remaining: self.len().map_err(invalid)?,
            }),
            sys::LITE3_TYPE_ARRAY => visitor.visit_seq(Access {
                entries: self.iter().map_err(invalid)?,
                pending: None,
                remaining: self.len().map_err(invalid)?,
            }),
            _ => Err(invalid("unsupported value kind")),
        }
    }
    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = self.owner.node(self.offset);
        match node.kind {
            sys::LITE3_TYPE_F64 => {
                let value = node.real as f32;
                if !value.is_finite() {
                    return Err(invalid("number exceeds f32 range"));
                }
                visitor.visit_f32(value)
            }
            sys::LITE3_TYPE_I64 => visitor.visit_i64(node.integer),
            _ => self.deserialize_any(visitor),
        }
    }
    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        if self.owner.node(self.offset).kind == sys::LITE3_TYPE_NULL {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_newtype_struct(self)
    }
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        if let Some(text) = self.text() {
            return visitor
                .visit_enum(de::value::BorrowedStrDeserializer::<DecodeError>::new(text));
        }
        if self.owner.node(self.offset).kind != sys::LITE3_TYPE_OBJECT
            || self.len().map_err(invalid)? != 1
        {
            return Err(invalid("expected one enum variant"));
        }
        let (key, value) = self
            .iter()
            .map_err(invalid)?
            .next()
            .ok_or_else(|| invalid("missing enum variant"))?
            .map_err(invalid)?;
        visitor.visit_enum(Enum {
            key: key.ok_or_else(|| invalid("missing enum name"))?,
            value,
        })
    }
    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        // Acquisition already checked the entire immutable subtree.
        visitor.visit_unit()
    }
    serde::forward_to_deserialize_any! { bool i8 i16 i32 i64 u8 u16 u32 u64 i128 u128 f64 char str string bytes byte_buf unit unit_struct seq tuple tuple_struct map struct identifier }
}

#[cfg(test)]
mod tests {
    use crate::{
        core::lite3::{Lite3Buf, sys},
        view::MessageView,
    };
    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct Reading<'a> {
        name: &'a str,
        samples: Vec<i16>,
        optional: Option<i64>,
        mode: Mode,
    }
    #[derive(Debug, serde::Deserialize, PartialEq)]
    enum Mode {
        Idle,
        Active { count: i64 },
        Pair(i16, i16),
        Label(String),
    }
    #[test]
    fn borrowed_fields_and_enum_shapes_decode_without_json() {
        for mode in [
            r#""Idle""#,
            r#"{"Active":{"count":7}}"#,
            r#"{"Pair":[2,3]}"#,
            r#"{"Label":"x"}"#,
        ] {
            let json = format!(
                r#"{{"meta":{{"tags":[]}},"data":{{"name":"sensor","samples":[-3,7],"optional":null,"mode":{mode},"ignored":{{"deep":[1,2]}}}}}}"#
            );
            let encoded = Lite3Buf::from_json_str(&json).unwrap();
            let view = MessageView::from_bytes(encoded.as_slice(), 1, 1).unwrap();
            let reading: Reading<'_> = view.data().decode().unwrap();
            let data_json =
                serde_json::from_str::<serde_json::Value>(&json).unwrap()["data"].to_string();
            let expected: Reading<'_> = serde_json::from_str(&data_json).unwrap();
            assert_eq!(reading, expected);
            assert_eq!(reading.name, "sensor");
            assert_eq!(reading.samples, [-3, 7]);
            assert_eq!(reading.optional, None);
            assert_eq!(
                reading.name.as_ptr(),
                view.data()
                    .get("name")
                    .unwrap()
                    .unwrap()
                    .text()
                    .unwrap()
                    .as_ptr()
            );
        }
    }
    #[test]
    fn numeric_range_and_type_errors_remain_errors() {
        let bytes = Lite3Buf::from_json_str(
            r#"{"meta":{"tags":[]},"data":{"n":256,"neg":-1,"real":1.25}}"#,
        )
        .unwrap();
        let view = MessageView::from_bytes(bytes.as_slice(), 1, 0).unwrap();
        assert!(
            view.data()
                .get("n")
                .unwrap()
                .unwrap()
                .decode::<u8>()
                .is_err()
        );
        assert!(
            view.data()
                .get("neg")
                .unwrap()
                .unwrap()
                .decode::<u64>()
                .is_err()
        );
        assert!(
            view.data()
                .get("real")
                .unwrap()
                .unwrap()
                .decode::<i64>()
                .is_err()
        );
    }
    #[test]
    fn narrowing_float_overflow_is_a_range_error() {
        let owner = MessageView::from_json(br#"{"large":1e100,"normal":0.1}"#, &[]).unwrap();
        assert!(serde_json::from_str::<f32>("1e100").is_err());
        assert!(
            owner
                .data()
                .get("large")
                .unwrap()
                .unwrap()
                .decode::<f32>()
                .is_err()
        );
        assert_eq!(
            owner
                .data()
                .get("normal")
                .unwrap()
                .unwrap()
                .decode::<f32>()
                .unwrap(),
            0.1_f32
        );
    }
    #[test]
    fn byte_fields_borrow_the_validated_owner() {
        let mut builder = crate::core::lite3::builder::Builder::new(&[]).unwrap();
        let bytes = b"\0native\xff";
        builder
            .put(
                builder.data,
                Some("blob"),
                sys::ValueView {
                    kind: sys::LITE3_TYPE_BYTES,
                    text: bytes.as_ptr().cast(),
                    text_len: bytes.len(),
                    ..Default::default()
                },
            )
            .unwrap();
        let owner = builder.finish().unwrap();
        let value = owner.data().get("blob").unwrap().unwrap();
        let borrowed: &[u8] = value.decode().unwrap();
        assert_eq!(borrowed, bytes);
        assert_eq!(borrowed.as_ptr(), value.span().unwrap().as_ptr());
    }
}
