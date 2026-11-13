//! Serialize once into the growing Lite3 writer, including custom Serde hooks.
use super::{MessageView, ValueRef};
use crate::core::{
    error::{Error, ErrorKind},
    lite3::{builder::Builder, sys},
};
use serde::ser::{self, Serialize, SerializeSeq};

type CodecError = serde::de::value::Error;
fn invalid(message: impl std::fmt::Display) -> CodecError {
    ser::Error::custom(message)
}
impl MessageView {
    /// Parse explicit JSON input once using the new exact integer contract.
    pub fn from_json(data: &[u8], tags: &[&str]) -> Result<Self, Error> {
        crate::core::lite3::ParsedJson::parse_view(data)?.encode_message_view(tags)
    }
    /// Encode native data once. Storage assigns sequence/time when appending.
    pub fn encode<T: Serialize + ?Sized>(data: &T, tags: &[&str]) -> Result<Self, Error> {
        let mut builder = Builder::new_without_data(tags)?;
        data.serialize(Writer {
            builder: &mut builder,
            parent: 0,
            key: Some("data"),
            depth: 1,
        })
        .map_err(|e| Error::new(ErrorKind::Usage).with_message(e.to_string()))?;
        builder.finish()
    }
}
impl Serialize for ValueRef<'_> {
    fn serialize<S: ser::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Share the checked traversal; native bytes keep their Serde byte type.
        super::json::Ordered {
            value: *self,
            native_bytes: true,
        }
        .serialize(serializer)
    }
}
pub(crate) fn copy_value(
    builder: &mut Builder,
    parent: usize,
    key: Option<&str>,
    value: ValueRef<'_>,
) -> Result<(), Error> {
    value
        .serialize(Writer {
            builder,
            parent,
            key,
            depth: 1,
        })
        .map_err(|e| Error::new(ErrorKind::Usage).with_message(e.to_string()))
}
struct Writer<'a> {
    builder: &'a mut Builder,
    parent: usize,
    key: Option<&'a str>,
    depth: usize,
}
impl<'a> Writer<'a> {
    fn put(self, value: sys::ValueView) -> Result<(), CodecError> {
        self.builder
            .put(self.parent, self.key, value)
            .map(|_| ())
            .map_err(invalid)
    }
    fn container(self, array: bool) -> Result<Compound<'a>, CodecError> {
        if self.depth >= 32 {
            return Err(invalid("container nesting exceeds limit"));
        }
        let parent = self
            .builder
            .container(self.parent, self.key, array)
            .map_err(invalid)?;
        Ok(Compound {
            builder: self.builder,
            parent,
            depth: self.depth + 1,
            key: None,
        })
    }
    fn variant(self, variant: &str, array: bool) -> Result<Compound<'a>, CodecError> {
        let mut outer = self.container(false)?;
        let depth = outer.depth;
        if depth >= 32 {
            return Err(invalid("container nesting exceeds limit"));
        }
        let parent = outer
            .builder
            .container(outer.parent, Some(variant), array)
            .map_err(invalid)?;
        outer.parent = parent;
        outer.depth += 1;
        Ok(outer)
    }
}
struct Compound<'a> {
    builder: &'a mut Builder,
    parent: usize,
    depth: usize,
    key: Option<MapKey>,
}
impl Compound<'_> {
    fn child<T: Serialize + ?Sized>(
        &mut self,
        key: Option<&str>,
        value: &T,
    ) -> Result<(), CodecError> {
        value.serialize(Writer {
            builder: self.builder,
            parent: self.parent,
            key,
            depth: self.depth,
        })
    }
}
impl<'a> ser::Serializer for Writer<'a> {
    type Ok = ();
    type Error = CodecError;
    type SerializeSeq = Compound<'a>;
    type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>;
    type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>;
    type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;
    fn serialize_bool(self, value: bool) -> Result<(), CodecError> {
        self.put(sys::ValueView {
            kind: sys::LITE3_TYPE_BOOL,
            integer: i64::from(value),
            ..Default::default()
        })
    }
    fn serialize_i8(self, value: i8) -> Result<(), CodecError> {
        self.serialize_i64(value.into())
    }
    fn serialize_i16(self, value: i16) -> Result<(), CodecError> {
        self.serialize_i64(value.into())
    }
    fn serialize_i32(self, value: i32) -> Result<(), CodecError> {
        self.serialize_i64(value.into())
    }
    fn serialize_i64(self, value: i64) -> Result<(), CodecError> {
        self.put(sys::ValueView {
            kind: sys::LITE3_TYPE_I64,
            integer: value,
            ..Default::default()
        })
    }
    fn serialize_i128(self, value: i128) -> Result<(), CodecError> {
        self.serialize_i64(
            value
                .try_into()
                .map_err(|_| invalid("integer exceeds signed 64-bit range"))?,
        )
    }
    fn serialize_u8(self, value: u8) -> Result<(), CodecError> {
        self.serialize_i64(value.into())
    }
    fn serialize_u16(self, value: u16) -> Result<(), CodecError> {
        self.serialize_i64(value.into())
    }
    fn serialize_u32(self, value: u32) -> Result<(), CodecError> {
        self.serialize_i64(value.into())
    }
    fn serialize_u64(self, value: u64) -> Result<(), CodecError> {
        self.serialize_i64(
            value
                .try_into()
                .map_err(|_| invalid("integer exceeds signed 64-bit range"))?,
        )
    }
    fn serialize_u128(self, value: u128) -> Result<(), CodecError> {
        self.serialize_i64(
            value
                .try_into()
                .map_err(|_| invalid("integer exceeds signed 64-bit range"))?,
        )
    }
    fn serialize_f32(self, value: f32) -> Result<(), CodecError> {
        self.serialize_f64(value.into())
    }
    fn serialize_f64(self, value: f64) -> Result<(), CodecError> {
        if !value.is_finite() {
            return Err(invalid("number must be finite"));
        }
        self.put(sys::ValueView {
            kind: sys::LITE3_TYPE_F64,
            real: value,
            ..Default::default()
        })
    }
    fn serialize_char(self, value: char) -> Result<(), CodecError> {
        self.serialize_str(value.encode_utf8(&mut [0; 4]))
    }
    fn serialize_str(self, value: &str) -> Result<(), CodecError> {
        self.put(sys::ValueView {
            kind: sys::LITE3_TYPE_STRING,
            text: value.as_ptr().cast(),
            text_len: value.len(),
            ..Default::default()
        })
    }
    fn serialize_bytes(self, value: &[u8]) -> Result<(), CodecError> {
        self.put(sys::ValueView {
            kind: sys::LITE3_TYPE_BYTES,
            text: value.as_ptr().cast(),
            text_len: value.len(),
            ..Default::default()
        })
    }
    fn serialize_none(self) -> Result<(), CodecError> {
        self.serialize_unit()
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), CodecError> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), CodecError> {
        self.put(sys::ValueView {
            kind: sys::LITE3_TYPE_NULL,
            ..Default::default()
        })
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), CodecError> {
        self.serialize_unit()
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<(), CodecError> {
        self.serialize_str(variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<(), CodecError> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), CodecError> {
        self.container(false)?.child(Some(variant), value)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, CodecError> {
        self.container(true)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, CodecError> {
        self.container(true)
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, CodecError> {
        self.container(true)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, CodecError> {
        self.variant(variant, true)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, CodecError> {
        self.container(false)
    }
    fn serialize_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStruct, CodecError> {
        self.container(false)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, CodecError> {
        self.variant(variant, false)
    }
}
impl ser::SerializeSeq for Compound<'_> {
    type Ok = ();
    type Error = CodecError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), CodecError> {
        self.child(None, value)
    }
    fn end(self) -> Result<(), CodecError> {
        Ok(())
    }
}
impl ser::SerializeTuple for Compound<'_> {
    type Ok = ();
    type Error = CodecError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), CodecError> {
        SerializeSeq::serialize_element(self, value)
    }
    fn end(self) -> Result<(), CodecError> {
        Ok(())
    }
}
impl ser::SerializeTupleStruct for Compound<'_> {
    type Ok = ();
    type Error = CodecError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), CodecError> {
        self.child(None, value)
    }
    fn end(self) -> Result<(), CodecError> {
        Ok(())
    }
}
impl ser::SerializeTupleVariant for Compound<'_> {
    type Ok = ();
    type Error = CodecError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), CodecError> {
        self.child(None, value)
    }
    fn end(self) -> Result<(), CodecError> {
        Ok(())
    }
}
impl ser::SerializeMap for Compound<'_> {
    type Ok = ();
    type Error = CodecError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), CodecError> {
        if self.key.is_some() {
            return Err(invalid("map key has no value"));
        }
        self.key = Some(key.serialize(KeySerializer)?);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), CodecError> {
        let key = self
            .key
            .take()
            .ok_or_else(|| invalid("map value has no key"))?;
        self.child(Some(key.as_str()), value)
    }
    fn serialize_entry<K: Serialize + ?Sized, V: Serialize + ?Sized>(
        &mut self,
        key: &K,
        value: &V,
    ) -> Result<(), CodecError> {
        let key = key.serialize(KeySerializer)?;
        self.child(Some(key.as_str()), value)
    }
    fn end(self) -> Result<(), CodecError> {
        if self.key.is_some() {
            Err(invalid("map key has no value"))
        } else {
            Ok(())
        }
    }
}
impl ser::SerializeStruct for Compound<'_> {
    type Ok = ();
    type Error = CodecError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), CodecError> {
        self.child(Some(key), value)
    }
    fn end(self) -> Result<(), CodecError> {
        Ok(())
    }
}
impl ser::SerializeStructVariant for Compound<'_> {
    type Ok = ();
    type Error = CodecError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), CodecError> {
        self.child(Some(key), value)
    }
    fn end(self) -> Result<(), CodecError> {
        Ok(())
    }
}

// Keep short serialized map keys owned until their hook returns, without
// allocating. This preserves key-hook/value-hook ordering for custom types.
enum MapKey {
    Inline { bytes: [u8; 64], len: u8 },
    Heap(String),
}
impl MapKey {
    fn text(value: &str) -> Self {
        if value.len() < 64 {
            let mut bytes = [0; 64];
            bytes[..value.len()].copy_from_slice(value.as_bytes());
            Self::Inline {
                bytes,
                len: value.len() as u8,
            }
        } else {
            Self::Heap(value.to_owned())
        }
    }
    fn as_str(&self) -> &str {
        match self {
            // Only complete UTF-8 strings enter the inline buffer.
            Self::Inline { bytes, len } => unsafe {
                std::str::from_utf8_unchecked(&bytes[..usize::from(*len)])
            },
            Self::Heap(value) => value,
        }
    }
    fn formatted<T: std::fmt::Display + ?Sized>(value: &T) -> Result<Self, CodecError> {
        use std::fmt::Write as _;
        let mut key = Self::text("");
        write!(&mut key, "{value}").map_err(invalid)?;
        Ok(key)
    }
}
impl std::fmt::Write for MapKey {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        match self {
            Self::Inline { bytes, len } if value.len() < 64 - usize::from(*len) => {
                let start = usize::from(*len);
                bytes[start..start + value.len()].copy_from_slice(value.as_bytes());
                *len += value.len() as u8;
            }
            Self::Inline { .. } => {
                let mut buffer = String::with_capacity(self.as_str().len() + value.len());
                buffer.push_str(self.as_str());
                buffer.push_str(value);
                *self = Self::Heap(buffer);
            }
            Self::Heap(buffer) => buffer.push_str(value),
        }
        Ok(())
    }
}
struct KeySerializer;
macro_rules! key_number {
    ($($name:ident: $kind:ty),* $(,)?) => { $(fn $name(self, value: $kind) -> Result<MapKey, CodecError> { MapKey::formatted(&value) })* };
}
macro_rules! invalid_key {
    ($name:ident($($arg:ident: $ty:ty),* $(,)?) -> $out:ty) => {
        fn $name(self, $(_: $ty),*) -> Result<$out, CodecError> { Err(invalid("map key must be a string or scalar")) }
    };
}
impl ser::Serializer for KeySerializer {
    type Ok = MapKey;
    type Error = CodecError;
    type SerializeSeq = ser::Impossible<MapKey, CodecError>;
    type SerializeTuple = Self::SerializeSeq;
    type SerializeTupleStruct = Self::SerializeSeq;
    type SerializeTupleVariant = Self::SerializeSeq;
    type SerializeMap = Self::SerializeSeq;
    type SerializeStruct = Self::SerializeSeq;
    type SerializeStructVariant = Self::SerializeSeq;
    key_number!(serialize_bool: bool, serialize_i8: i8, serialize_i16: i16, serialize_i32: i32, serialize_i64: i64, serialize_i128: i128, serialize_u8: u8, serialize_u16: u16, serialize_u32: u32, serialize_u64: u64, serialize_u128: u128, serialize_char: char);
    fn serialize_f32(self, value: f32) -> Result<MapKey, CodecError> {
        if value.is_finite() {
            serde_json::to_string(&value)
                .map(MapKey::Heap)
                .map_err(invalid)
        } else {
            Err(invalid("map key must be finite"))
        }
    }
    fn serialize_f64(self, value: f64) -> Result<MapKey, CodecError> {
        if value.is_finite() {
            serde_json::to_string(&value)
                .map(MapKey::Heap)
                .map_err(invalid)
        } else {
            Err(invalid("map key must be finite"))
        }
    }
    fn collect_str<T: std::fmt::Display + ?Sized>(self, value: &T) -> Result<MapKey, CodecError> {
        MapKey::formatted(value)
    }
    fn serialize_str(self, value: &str) -> Result<MapKey, CodecError> {
        Ok(MapKey::text(value))
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<MapKey, CodecError> {
        Ok(MapKey::text(variant))
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<MapKey, CodecError> {
        value.serialize(self)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<MapKey, CodecError> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<MapKey, CodecError> {
        Err(invalid("enum map key is unsupported"))
    }
    invalid_key!(serialize_bytes(value: &[u8]) -> MapKey);
    invalid_key!(serialize_none() -> MapKey);
    invalid_key!(serialize_unit() -> MapKey);
    invalid_key!(serialize_unit_struct(name: &'static str) -> MapKey);
    invalid_key!(serialize_seq(len: Option<usize>) -> Self::SerializeSeq);
    invalid_key!(serialize_tuple(len: usize) -> Self::SerializeTuple);
    invalid_key!(
        serialize_tuple_struct(name: &'static str, len: usize) -> Self::SerializeTupleStruct
    );
    invalid_key!(
        serialize_tuple_variant(
            name: &'static str,
            index: u32,
            variant: &'static str,
            len: usize,
        ) -> Self::SerializeTupleVariant
    );
    invalid_key!(serialize_map(len: Option<usize>) -> Self::SerializeMap);
    invalid_key!(serialize_struct(name: &'static str, len: usize) -> Self::SerializeStruct);
    invalid_key!(
        serialize_struct_variant(
            name: &'static str,
            index: u32,
            variant: &'static str,
            len: usize,
        ) -> Self::SerializeStructVariant
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, collections::BTreeMap};
    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    enum Mode {
        Idle,
        Active { count: i64 },
        Pair(i16, i16),
        Label(String),
    }
    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Record {
        name: String,
        values: Vec<i64>,
        optional: Option<bool>,
        mode: Mode,
        keys: BTreeMap<i32, String>,
    }
    #[test]
    fn typed_roundtrips_and_json_shapes_match() {
        for mode in [
            Mode::Idle,
            Mode::Active { count: i64::MIN },
            Mode::Pair(-2, 3),
            Mode::Label("a\0b".into()),
        ] {
            let record = Record {
                name: "sensor".into(),
                values: vec![i64::MIN, 0, i64::MAX],
                optional: None,
                mode,
                keys: BTreeMap::from([(7, "seven".into())]),
            };
            let view = MessageView::encode(&record, &["sensor\0tag"]).unwrap();
            assert_eq!(view.data().decode::<Record>().unwrap(), record);
            assert_eq!(
                view.data().decode::<serde_json::Value>().unwrap(),
                serde_json::to_value(&record).unwrap()
            );
        }
    }
    #[test]
    fn short_and_long_keys_keep_the_same_values() {
        let input = BTreeMap::from([
            (String::new(), 0),
            ("x".repeat(63), 1),
            ("x".repeat(64), 2),
            ("é".repeat(31), 3),
            ("é".repeat(32), 4),
            ("long".repeat(5000), 5),
        ]);
        let view = MessageView::encode(&input, &[]).unwrap();
        assert_eq!(
            view.data().decode::<BTreeMap<String, i64>>().unwrap(),
            input
        );
    }
    #[test]
    fn key_hook_finishes_before_value_hook() {
        use serde::ser::SerializeMap as _;
        use std::cell::RefCell;
        struct KeyHook<'a>(&'a RefCell<Vec<&'static str>>);
        impl Serialize for KeyHook<'_> {
            fn serialize<S: ser::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.0.borrow_mut().push("key-start");
                let result = serializer.serialize_str("name");
                self.0.borrow_mut().push("key-end");
                result
            }
        }
        struct ValueHook<'a>(&'a RefCell<Vec<&'static str>>);
        impl Serialize for ValueHook<'_> {
            fn serialize<S: ser::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.0.borrow_mut().push("value");
                serializer.serialize_i64(7)
            }
        }
        struct Input<'a>(&'a RefCell<Vec<&'static str>>);
        impl Serialize for Input<'_> {
            fn serialize<S: ser::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(&KeyHook(self.0), &ValueHook(self.0))?;
                map.end()
            }
        }
        let events = RefCell::new(Vec::new());
        let view = MessageView::encode(&Input(&events), &[]).unwrap();
        assert_eq!(*events.borrow(), ["key-start", "key-end", "value"]);
        assert_eq!(view.data().get("name").unwrap().unwrap().integer(), Some(7));
    }
    #[test]
    fn explicit_json_records_safe_conversion_during_encoding() {
        for (number, safe) in [
            ("9007199254740991", true),
            ("9007199254740992", false),
            ("-9007199254740992", false),
            ("9223372036854775807", false),
            ("1e20", true),
            ("-0.0", true),
        ] {
            let json = format!("{{\"n\":{number}}}");
            let view = MessageView::from_json(json.as_bytes(), &[]).unwrap();
            assert_eq!(view.json_native_safe(), safe, "{number}");
            assert!(view.retained_bytes() >= view.bytes().len());
            let forwarded = view.with_identity(2, 3, 0);
            assert_eq!(forwarded.retained_bytes(), view.retained_bytes());
            assert_eq!(forwarded.bytes().as_ptr(), view.bytes().as_ptr());
        }
        assert_eq!(
            unsafe { crate::abi_views::plsm_view_retained_bytes(std::ptr::null()) },
            0
        );
    }
    struct Counted<'a> {
        calls: &'a Cell<usize>,
        text: &'a str,
    }
    impl Serialize for Counted<'_> {
        fn serialize<S: ser::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.calls.set(self.calls.get() + 1);
            serializer.serialize_str(self.text)
        }
    }
    #[test]
    fn growth_does_not_replay_hooks() {
        let text = "x".repeat(128 * 1024);
        let calls = Cell::new(0);
        let map = BTreeMap::from([
            (
                "first",
                Counted {
                    calls: &calls,
                    text: &text,
                },
            ),
            (
                "second",
                Counted {
                    calls: &calls,
                    text: &text,
                },
            ),
        ]);
        let view = MessageView::encode(&map, &[]).unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(
            view.data().get("second").unwrap().unwrap().text(),
            Some(text.as_str())
        );
    }
    #[test]
    fn invalid_native_values_and_nesting_fail() {
        assert!(MessageView::encode(&BTreeMap::from([("n", f64::INFINITY)]), &[]).is_err());
        assert!(MessageView::encode(&BTreeMap::from([("n", u64::MAX)]), &[]).is_err());
        assert!(MessageView::encode(&BTreeMap::from([("bad\0key", 1)]), &[]).is_err());
        assert!(MessageView::encode(&vec![1, 2], &[]).is_err());
        let mut value = serde_json::json!(0);
        for _ in 0..32 {
            value = serde_json::json!({"x":value});
        }
        assert!(MessageView::encode(&value, &[]).is_err());
        let view = MessageView::encode(&BTreeMap::from([("zero", -0.0_f64)]), &[]).unwrap();
        assert_eq!(
            view.data()
                .get("zero")
                .unwrap()
                .unwrap()
                .real()
                .unwrap()
                .to_bits(),
            (-0.0_f64).to_bits()
        );
    }
    struct Blob<'a>(&'a [u8]);
    impl Serialize for Blob<'_> {
        fn serialize<S: ser::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bytes(self.0)
        }
    }
    #[test]
    fn native_bytes_remain_bytes() {
        let bytes = b"\0raw\xff";
        let view = MessageView::encode(&BTreeMap::from([("blob", Blob(bytes))]), &[]).unwrap();
        let decoded: &[u8] = view.data().get("blob").unwrap().unwrap().decode().unwrap();
        assert_eq!(decoded, bytes);
    }
}

#[cfg(test)]
mod pool_tests {
    use super::*;
    use crate::api::{AppendOptions, Durability, Pool, PoolApiExt, PoolOptions};
    #[test]
    fn native_append_and_forward_share_an_independent_encoding() {
        let temp = tempfile::tempdir().unwrap();
        let mut pool = Pool::create(
            temp.path().join("typed.plasmite"),
            PoolOptions::new(1024 * 1024),
        )
        .unwrap();
        let options = AppendOptions::new(19, Durability::Fast);
        let first = pool
            .append_value(
                &serde_json::json!({"n":7,"text":"sensor"}),
                &["first"],
                options,
            )
            .unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(first.timestamp_ns, 19);
        assert!(first.json_native_safe());
        let second = pool
            .append_view(&first, AppendOptions::new(23, Durability::Fast))
            .unwrap();
        assert_eq!(second.seq, 2);
        assert_eq!(second.timestamp_ns, 23);
        assert_eq!(first.bytes().as_ptr(), second.bytes().as_ptr());
        let read = pool.get_view(2).unwrap();
        assert_eq!(
            read.data()
                .get("n")
                .unwrap()
                .unwrap()
                .decode::<i64>()
                .unwrap(),
            7
        );
        let changed = MessageView::encode(&first.data(), &["changed"]).unwrap();
        assert_eq!(
            changed.data().to_json().unwrap(),
            first.data().to_json().unwrap()
        );
        assert_eq!(
            changed
                .root()
                .get("meta")
                .unwrap()
                .unwrap()
                .get("tags")
                .unwrap()
                .unwrap()
                .index(0)
                .unwrap()
                .unwrap()
                .text(),
            Some("changed")
        );
        let count = pool.bounds().unwrap().newest_seq;
        assert!(
            pool.append_value(&serde_json::json!({"bad\0key":1}), &[], options)
                .is_err()
        );
        assert_eq!(pool.bounds().unwrap().newest_seq, count);
        drop(pool);
        assert_eq!(
            first.data().get("text").unwrap().unwrap().text(),
            Some("sensor")
        );
    }
}
