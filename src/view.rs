//! Immutable, aligned Lite3 snapshots and borrowed node references.
#![allow(clippy::result_large_err)]
use crate::core::error::{Error, ErrorKind};
use crate::core::lite3::{JsonValue, Lite3DocRef, sys};
use std::collections::HashMap;

fn corrupt() -> Error {
    Error::new(ErrorKind::Corrupt).with_message("invalid Lite3 view")
}

enum Storage {
    Bytes(Vec<u8>),
    Aligned { words: Vec<u32>, len: usize },
}
impl Storage {
    fn new(bytes: Vec<u8>) -> Self {
        if bytes.as_ptr().align_offset(4) == 0 {
            return Self::Bytes(bytes);
        }
        let mut words = vec![0u32; bytes.len().div_ceil(4)];
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), words.as_mut_ptr().cast(), bytes.len());
        }
        Self::Aligned {
            words,
            len: bytes.len(),
        }
    }
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Aligned { words, len } => unsafe {
                std::slice::from_raw_parts(words.as_ptr().cast(), *len)
            },
        }
    }
}

/// A sealed snapshot; construction consumes private bytes or copies caller bytes.
#[derive(Clone)]
pub struct MessageView {
    snapshot: std::sync::Arc<Snapshot>,
    pub seq: u64,
    pub timestamp_ns: u64,
    pub flags: u32,
}

struct Snapshot {
    storage: Storage,
    // Only objects containing duplicate names need a lookup correction.
    duplicates: HashMap<usize, HashMap<String, usize>>,
}

/// A cheap reference whose lifetime cannot outlive its snapshot.
#[derive(Clone, Copy)]
pub struct ValueRef<'a> {
    owner: &'a MessageView,
    offset: usize,
}

impl MessageView {
    pub fn from_bytes(bytes: &[u8], seq: u64, timestamp_ns: u64) -> Result<Self, Error> {
        Self::from_owned(bytes.to_vec(), seq, timestamp_ns, 0)
    }
    pub(crate) fn from_owned(
        bytes: Vec<u8>,
        seq: u64,
        timestamp_ns: u64,
        flags: u32,
    ) -> Result<Self, Error> {
        let mut owner = Self {
            snapshot: std::sync::Arc::new(Snapshot {
                storage: Storage::new(bytes),
                duplicates: HashMap::new(),
            }),
            seq,
            timestamp_ns,
            flags,
        };
        owner.doc().validate_view()?;
        owner.collect_duplicates(0)?;
        // Shape must agree with canonical last-key lookup as well as raw lookup.
        let meta = owner.root().get("meta")?.ok_or_else(corrupt)?;
        let data = owner.root().get("data")?.ok_or_else(corrupt)?;
        let tags = meta.get("tags")?.ok_or_else(corrupt)?;
        if meta.kind() != sys::LITE3_TYPE_OBJECT
            || data.kind() != sys::LITE3_TYPE_OBJECT
            || tags.kind() != sys::LITE3_TYPE_ARRAY
        {
            return Err(corrupt());
        }
        for (_, tag) in tags.entries()? {
            if tag.kind() != sys::LITE3_TYPE_STRING {
                return Err(corrupt());
            }
        }
        Ok(owner)
    }
    /// Retain a private checked builder allocation without copying or rescanning.
    ///
    /// # Safety
    /// The initialized prefix must be a complete readable message with canonical
    /// unique keys, valid UTF-8/finite numbers, and the common depth/shape limits.
    pub(crate) unsafe fn from_encoded_words(
        words: Vec<u32>,
        len: usize,
        seq: u64,
        timestamp_ns: u64,
        flags: u32,
    ) -> Self {
        assert!(len <= words.len() * 4);
        Self {
            snapshot: std::sync::Arc::new(Snapshot {
                storage: Storage::Aligned { words, len },
                duplicates: HashMap::new(),
            }),
            seq,
            timestamp_ns,
            flags,
        }
    }
    /// Reuse immutable payload ownership with a new committed identity.
    pub(crate) fn with_identity(&self, seq: u64, timestamp_ns: u64, flags: u32) -> Self {
        Self {
            snapshot: self.snapshot.clone(),
            seq,
            timestamp_ns,
            flags,
        }
    }
    pub fn bytes(&self) -> &[u8] {
        self.snapshot.storage.bytes()
    }
    fn doc(&self) -> Lite3DocRef<'_> {
        Lite3DocRef::new(self.bytes())
    }
    pub(crate) unsafe fn value_at(&self, offset: usize) -> ValueRef<'_> {
        ValueRef {
            owner: self,
            offset,
        }
    }
    pub fn root(&self) -> ValueRef<'_> {
        ValueRef {
            owner: self,
            offset: 0,
        }
    }
    pub fn data(&self) -> ValueRef<'_> {
        self.root()
            .get("data")
            .expect("checked lookup")
            .expect("checked data")
    }
    fn raw_entries(&self, offset: usize) -> Result<Vec<sys::Entry>, Error> {
        let count = self.doc().count_at(offset)? as usize;
        if count > self.bytes().len() {
            return Err(corrupt());
        }
        let mut entries = vec![sys::Entry::default(); count];
        if unsafe {
            sys::plasmite_lite3_entries(
                self.bytes().as_ptr(),
                self.bytes().len(),
                offset,
                entries.as_mut_ptr(),
                count,
            )
        } < 0
        {
            return Err(corrupt());
        }
        Ok(entries)
    }
    fn collect_duplicates(&mut self, offset: usize) -> Result<(), Error> {
        let entries = self.raw_entries(offset)?;
        let object = self.node(offset).kind == sys::LITE3_TYPE_OBJECT;
        let mut seen = HashMap::new();
        let mut duplicate = false;
        for entry in entries {
            if object {
                let key = unsafe {
                    std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                        entry.key.cast(),
                        entry.key_len,
                    ))
                };
                duplicate |= seen.insert(key, entry.offset).is_some();
            }
            if matches!(
                self.node(entry.offset).kind,
                sys::LITE3_TYPE_OBJECT | sys::LITE3_TYPE_ARRAY
            ) {
                self.collect_duplicates(entry.offset)?;
            }
        }
        if duplicate {
            std::sync::Arc::get_mut(&mut self.snapshot)
                .expect("unpublished owner")
                .duplicates
                .insert(
                    offset,
                    seen.into_iter()
                        .map(|(key, offset)| (key.to_owned(), offset))
                        .collect(),
                );
        }
        Ok(())
    }
    fn node(&self, offset: usize) -> sys::ValueView {
        let mut v = sys::ValueView::default();
        unsafe {
            sys::plasmite_lite3_value_view(self.bytes().as_ptr(), offset, &mut v);
        }
        v
    }
}
impl<'a> ValueRef<'a> {
    pub fn offset(self) -> usize {
        self.offset
    }
    pub fn kind(self) -> u8 {
        self.owner.node(self.offset).kind
    }
    pub fn get(self, key: &str) -> Result<Option<Self>, Error> {
        if self.kind() != sys::LITE3_TYPE_OBJECT {
            return Err(Error::new(ErrorKind::Usage).with_message("value is not an object"));
        }
        if key.contains('\0') {
            return Ok(None);
        }
        if let Some(keys) = self.owner.snapshot.duplicates.get(&self.offset) {
            return Ok(keys.get(key).map(|&offset| Self {
                owner: self.owner,
                offset,
            }));
        }
        match self.owner.doc().key_offset_at(self.offset, key) {
            Ok(offset) => Ok(Some(Self {
                owner: self.owner,
                offset,
            })),
            Err(_) => Ok(None),
        }
    }
    pub fn index(self, index: u32) -> Result<Option<Self>, Error> {
        if self.kind() != sys::LITE3_TYPE_ARRAY {
            return Err(Error::new(ErrorKind::Usage).with_message("value is not an array"));
        }
        if index >= self.len()? as u32 {
            return Ok(None);
        }
        let mut offset = 0;
        if unsafe {
            sys::plasmite_lite3_arr_get_val_ofs(
                self.owner.bytes().as_ptr(),
                self.owner.bytes().len(),
                self.offset,
                index,
                &mut offset,
            )
        } < 0
        {
            return Err(corrupt());
        }
        Ok(Some(Self {
            owner: self.owner,
            offset,
        }))
    }
    pub fn len(self) -> Result<usize, Error> {
        if let Some(keys) = self.owner.snapshot.duplicates.get(&self.offset) {
            return Ok(keys.len());
        }
        self.owner.doc().count_at(self.offset).map(|v| v as usize)
    }
    pub fn is_empty(self) -> Result<bool, Error> {
        self.len().map(|n| n == 0)
    }
    pub fn entries(self) -> Result<Vec<(Option<&'a str>, Self)>, Error> {
        let entries = self.owner.raw_entries(self.offset)?;
        let object = self.kind() == sys::LITE3_TYPE_OBJECT;
        let duplicates = self.owner.snapshot.duplicates.get(&self.offset);
        Ok(entries
            .into_iter()
            .filter_map(|entry| {
                let key = object.then(|| unsafe {
                    std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                        entry.key.cast(),
                        entry.key_len,
                    ))
                });
                if let (Some(keys), Some(key)) = (duplicates, key)
                    && keys.get(key) != Some(&entry.offset)
                {
                    return None;
                }
                Some((
                    key,
                    Self {
                        owner: self.owner,
                        offset: entry.offset,
                    },
                ))
            })
            .collect())
    }
    pub fn integer(self) -> Option<i64> {
        matches!(self.kind(), sys::LITE3_TYPE_I64 | sys::LITE3_TYPE_BOOL)
            .then(|| self.owner.node(self.offset).integer)
    }
    pub fn real(self) -> Option<f64> {
        (self.kind() == sys::LITE3_TYPE_F64).then(|| self.owner.node(self.offset).real)
    }
    pub fn text(self) -> Option<&'a str> {
        (self.kind() == sys::LITE3_TYPE_STRING)
            .then(|| unsafe { std::str::from_utf8_unchecked(self.span().expect("string span")) })
    }
    pub fn span(self) -> Option<&'a [u8]> {
        if !matches!(self.kind(), sys::LITE3_TYPE_STRING | sys::LITE3_TYPE_BYTES) {
            return None;
        }
        let node = self.owner.node(self.offset);
        Some(unsafe { std::slice::from_raw_parts(node.text.cast(), node.text_len) })
    }
    pub fn to_json(self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(&unsafe { JsonValue::from_validated(self.owner.doc(), self.offset) })
            .map_err(|_| corrupt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lite3::Lite3Buf;
    fn bytes() -> Vec<u8> {
        Lite3Buf::from_json_str(
            r#"{"meta":{"tags":[]},"data":{"metrics":{"power":7},"samples":[1,null,"x"]}}"#,
        )
        .unwrap()
        .as_slice()
        .to_vec()
    }
    #[test]
    fn views_keep_owned_bytes_and_natural_field_semantics() {
        let mut input = bytes();
        let view = MessageView::from_bytes(&input, 17, 22).unwrap();
        input.fill(0);
        assert_eq!(view.seq, 17);
        assert_eq!(view.bytes().as_ptr().align_offset(4), 0);
        let data = view.data();
        assert!(data.get("absent").unwrap().is_none());
        assert!(data.get("metrics\0extra").unwrap().is_none());
        assert_eq!(
            data.get("metrics")
                .unwrap()
                .unwrap()
                .get("power")
                .unwrap()
                .unwrap()
                .integer(),
            Some(7)
        );
        let samples = data.get("samples").unwrap().unwrap();
        assert_eq!(samples.len().unwrap(), 3);
        assert_eq!(
            samples.index(1).unwrap().unwrap().kind(),
            sys::LITE3_TYPE_NULL
        );
        assert_eq!(samples.index(2).unwrap().unwrap().text(), Some("x"));
        assert!(samples.index(3).unwrap().is_none());
        assert_eq!(samples.to_json().unwrap(), br#"[1,null,"x"]"#);
        assert_eq!(samples.entries().unwrap().len(), 3);
    }
    #[test]
    fn duplicate_keys_have_one_consistent_last_winner() {
        let encoded =
            Lite3Buf::from_json_str(r#"{"meta":{"tags":[]},"data":{"a":1,"b":2}}"#).unwrap();
        let initial = MessageView::from_bytes(encoded.as_slice(), 1, 1).unwrap();
        let entries = initial.raw_entries(initial.data().offset()).unwrap();
        let first = entries[0];
        let last = entries[1];
        let key = unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                first.key.cast(),
                first.key_len,
            ))
        }
        .to_owned();
        let mut changed = initial.bytes().to_vec();
        changed[last.key as usize - initial.bytes().as_ptr() as usize] = key.as_bytes()[0];
        let view = MessageView::from_owned(changed, 1, 1, 0).unwrap();
        let data = view.data();
        assert_eq!(data.len().unwrap(), 1);
        assert_eq!(data.entries().unwrap().len(), 1);
        let selected = data.get(&key).unwrap().unwrap().integer().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&data.to_json().unwrap()).unwrap();
        assert_eq!(json[&key].as_i64(), Some(selected));
        assert_eq!(selected, initial.node(last.offset).integer);
    }
    #[test]
    fn input_alignment_and_threaded_owner_are_explicit() {
        let encoded = bytes();
        let mut unaligned = vec![0];
        unaligned.extend_from_slice(&encoded);
        let owner = std::sync::Arc::new(MessageView::from_bytes(&unaligned[1..], 1, 2).unwrap());
        let child = owner.clone();
        drop(owner);
        std::thread::spawn(move || assert_eq!(child.data().len().unwrap(), 2))
            .join()
            .unwrap();
    }
}

#[cfg(test)]
mod read_into_tests {
    use super::*;
    use crate::core::{pool::Pool, pool::PoolOptions};
    #[test]
    fn read_into_reports_capacity_without_partial_payload() {
        let dir = tempfile::tempdir().unwrap();
        let mut pool = Pool::create(
            dir.path().join("test.plasmite"),
            PoolOptions::new(1024 * 1024),
        )
        .unwrap();
        let data =
            crate::core::lite3::Lite3Buf::from_json_str(r#"{"meta":{"tags":[]},"data":{"x":7}}"#)
                .unwrap();
        let seq = pool.append(data.as_slice()).unwrap();
        let mut small = [0x55; 4];
        let (required, _, _) = pool.get_into(seq, &mut small).unwrap();
        assert_eq!(required, data.len());
        assert_eq!(small, [0x55; 4]);
        let mut output = vec![0; required];
        let (len, time, flags) = pool.get_into(seq, &mut output).unwrap();
        drop(pool);
        let view = MessageView::from_owned(output, seq, time, flags).unwrap();
        assert_eq!(len, data.len());
        assert_eq!(view.data().get("x").unwrap().unwrap().integer(), Some(7));
    }
}
