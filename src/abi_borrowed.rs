//! Private synchronous reader for sealed Go-owned snapshots.
//!
//! Validation produces offset-only metadata. The binding keeps bytes and proof
//! private and immutable, passes them afresh for each call, and retains every Go
//! allocation until the call returns. No function stores a caller pointer.
#![allow(clippy::result_large_err)]
mod decode;
use crate::abi::{fail, plsm_buf, plsm_error, write_json_buf};
use crate::abi_views::PlsmSelector;
use crate::core::error::{Error, ErrorKind};
use crate::core::lite3::{JsonValue, Lite3DocRef, sys};

fn corrupt() -> Error {
    Error::new(ErrorKind::Corrupt).with_message("invalid borrowed Lite3 view")
}
fn usage() -> Error {
    Error::new(ErrorKind::Usage).with_message("invalid borrowed reader arguments")
}
fn finish(result: Result<(), Error>, error: *mut *mut plsm_error) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => fail(error, e),
    }
}
unsafe fn bytes<'a>(data: *const u8, len: usize) -> Result<&'a [u8], Error> {
    if data.is_null() || data.align_offset(4) != 0 || len > isize::MAX as usize {
        return Err(usage());
    }
    if len < 96 {
        return Err(corrupt());
    }
    Ok(unsafe { std::slice::from_raw_parts(data, len) })
}
unsafe fn offsets<'a>(data: *const usize, len: usize) -> Result<&'a [usize], Error> {
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() || len > isize::MAX as usize / size_of::<usize>() {
        return Err(usage());
    }
    Ok(unsafe { std::slice::from_raw_parts(data, len) })
}
struct Reader<'a> {
    doc: Lite3DocRef<'a>,
    duplicates: &'a [usize],
}
impl Reader<'_> {
    fn node(&self, offset: usize) -> Result<sys::ValueView, Error> {
        if offset >= self.doc.bytes().len() {
            return Err(corrupt());
        }
        let mut value = sys::ValueView::default();
        unsafe { sys::plasmite_lite3_value_view(self.doc.bytes().as_ptr(), offset, &mut value) };
        Ok(value)
    }
    fn get(&self, parent: usize, key: &str) -> Result<Option<usize>, Error> {
        if key.contains('\0') {
            return Ok(None);
        }
        if self.duplicates.binary_search(&parent).is_ok() {
            let mut state = [0u64; 32];
            if unsafe {
                sys::plasmite_lite3_cursor_init(
                    self.doc.bytes().as_ptr(),
                    self.doc.bytes().len(),
                    parent,
                    state.as_mut_ptr().cast(),
                    size_of_val(&state),
                )
            } < 0
            {
                return Err(corrupt());
            }
            let mut found = None;
            loop {
                let mut entry = sys::Entry::default();
                let mut hash = 0;
                match unsafe {
                    sys::plasmite_lite3_cursor_next(
                        self.doc.bytes().as_ptr(),
                        self.doc.bytes().len(),
                        state.as_mut_ptr().cast(),
                        &mut entry,
                        &mut hash,
                    )
                } {
                    0 => break,
                    1 => {
                        let name = unsafe {
                            std::slice::from_raw_parts(entry.key.cast::<u8>(), entry.key_len)
                        };
                        if name == key.as_bytes() {
                            found = Some(entry.offset);
                        }
                    }
                    _ => return Err(corrupt()),
                }
            }
            return Ok(found);
        }
        let mut offset = 0;
        let status = unsafe {
            sys::plasmite_lite3_lookup_span(
                self.doc.bytes().as_ptr(),
                self.doc.bytes().len(),
                parent,
                key.as_ptr().cast(),
                key.len(),
                &mut offset,
            )
        };
        if status == 0 {
            Ok(Some(offset))
        } else if unsafe { sys::plasmite_lite3_last_errno() } == libc::ENOMEM {
            Err(Error::new(ErrorKind::Internal).with_message("lookup allocation failed"))
        } else {
            Ok(None)
        }
    }
    fn packed(&self, offset: usize) -> Result<BorrowedValue, Error> {
        let mut output = BorrowedValue::default();
        let node = self.node(offset)?;
        output.kind = match node.kind {
            sys::LITE3_TYPE_NULL => 1,
            sys::LITE3_TYPE_BOOL => 2,
            sys::LITE3_TYPE_I64 => 3,
            sys::LITE3_TYPE_F64 => 4,
            sys::LITE3_TYPE_STRING => 5,
            sys::LITE3_TYPE_BYTES => 6,
            sys::LITE3_TYPE_OBJECT => 7,
            sys::LITE3_TYPE_ARRAY => 8,
            _ => return Err(corrupt()),
        };
        output.node = offset;
        output.integer = node.integer;
        output.real = node.real;
        if matches!(node.kind, sys::LITE3_TYPE_STRING | sys::LITE3_TYPE_BYTES) {
            output.offset = node.text as usize - self.doc.bytes().as_ptr() as usize;
            output.len = node.text_len;
        } else if matches!(node.kind, sys::LITE3_TYPE_OBJECT | sys::LITE3_TYPE_ARRAY) {
            output.len = self.count(offset)?;
        }
        Ok(output)
    }
    fn count(&self, offset: usize) -> Result<usize, Error> {
        let count = self.doc.count_at(offset)? as usize;
        if self.duplicates.binary_search(&offset).is_err() {
            return Ok(count);
        }
        let mut entries = vec![sys::Entry::default(); count];
        if unsafe {
            sys::plasmite_lite3_entries(
                self.doc.bytes().as_ptr(),
                self.doc.bytes().len(),
                offset,
                entries.as_mut_ptr(),
                count,
            )
        } < 0
        {
            return Err(corrupt());
        }
        let names = entries
            .iter()
            .map(|entry| unsafe {
                std::slice::from_raw_parts(entry.key.cast::<u8>(), entry.key_len)
            })
            .collect::<std::collections::HashSet<_>>();
        Ok(names.len())
    }
    fn lookup(&self, root: usize, tokens: &[String]) -> Result<Option<usize>, Error> {
        let mut current = root;
        for token in tokens {
            current = match self.node(current)?.kind {
                sys::LITE3_TYPE_OBJECT => match self.get(current, token)? {
                    Some(offset) => offset,
                    None => return Ok(None),
                },
                sys::LITE3_TYPE_ARRAY => {
                    if token.is_empty()
                        || (token.len() > 1 && token.starts_with('0'))
                        || !token.bytes().all(|b| b.is_ascii_digit())
                    {
                        return Err(usage());
                    }
                    let Ok(index) = token.parse::<u32>() else {
                        return Ok(None);
                    };
                    if index >= self.doc.count_at(current)? {
                        return Ok(None);
                    }
                    let mut offset = 0;
                    if unsafe {
                        sys::plasmite_lite3_arr_get_val_ofs(
                            self.doc.bytes().as_ptr(),
                            self.doc.bytes().len(),
                            current,
                            index,
                            &mut offset,
                        )
                    } < 0
                    {
                        return Err(corrupt());
                    }
                    offset
                }
                _ => return Ok(None),
            }
        }
        Ok(Some(current))
    }
}

/// Offset-only slots contain no Go or C pointers. Span offsets refer to the
/// sealed input, so the binding constructs requested strings exactly once.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BorrowedValue {
    pub kind: u32,
    pub reserved: u32,
    pub integer: i64,
    pub real: f64,
    pub node: usize,
    pub offset: usize,
    pub len: usize,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_go_validate(
    data: *const u8,
    len: usize,
    data_node: *mut usize,
    meta_node: *mut usize,
    duplicate_offsets: *mut plsm_buf,
    error: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            if data_node.is_null() || meta_node.is_null() || duplicate_offsets.is_null() {
                return Err(usage());
            }
            let input = unsafe { bytes(data, len) }?;
            let mut duplicates = Vec::<usize>::new();
            unsafe extern "C" fn duplicate(context: *mut std::ffi::c_void, offset: usize) -> i32 {
                unsafe { (&mut *context.cast::<Vec<usize>>()).push(offset) };
                0
            }
            let mut safe = 0;
            if unsafe {
                sys::plasmite_lite3_verify_view(
                    input.as_ptr(),
                    input.len(),
                    duplicate,
                    (&mut duplicates as *mut Vec<usize>).cast(),
                    &mut safe,
                )
            } < 0
            {
                return Err(corrupt());
            }
            duplicates.sort_unstable();
            let reader = Reader {
                doc: Lite3DocRef::new(input),
                duplicates: &duplicates,
            };
            if reader.node(0)?.kind != sys::LITE3_TYPE_OBJECT {
                return Err(corrupt());
            }
            let meta = reader.get(0, "meta")?.ok_or_else(corrupt)?;
            let root = reader.get(0, "data")?.ok_or_else(corrupt)?;
            if reader.node(meta)?.kind != sys::LITE3_TYPE_OBJECT
                || reader.node(root)?.kind != sys::LITE3_TYPE_OBJECT
            {
                return Err(corrupt());
            }
            let tags = reader.get(meta, "tags")?.ok_or_else(corrupt)?;
            if reader.node(tags)?.kind != sys::LITE3_TYPE_ARRAY {
                return Err(corrupt());
            }
            let mut state = [0u64; 32];
            if unsafe {
                sys::plasmite_lite3_cursor_init(
                    input.as_ptr(),
                    input.len(),
                    tags,
                    state.as_mut_ptr().cast(),
                    size_of_val(&state),
                )
            } < 0
            {
                return Err(corrupt());
            }
            loop {
                let mut entry = sys::Entry::default();
                let mut hash = 0;
                match unsafe {
                    sys::plasmite_lite3_cursor_next(
                        input.as_ptr(),
                        input.len(),
                        state.as_mut_ptr().cast(),
                        &mut entry,
                        &mut hash,
                    )
                } {
                    0 => break,
                    1 if reader.node(entry.offset)?.kind == sys::LITE3_TYPE_STRING => (),
                    _ => return Err(corrupt()),
                }
            }
            let proof = duplicates
                .iter()
                .flat_map(|offset| offset.to_ne_bytes())
                .collect();
            write_json_buf(duplicate_offsets, proof)?;
            unsafe {
                *data_node = root;
                *meta_node = meta;
            }
            Ok(())
        })(),
        error,
    )
}

/// Binding-private proof reuse. Input bytes and duplicate offsets must remain
/// unchanged since successful validate; every root comes from that same owner.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_go_select(
    data: *const u8,
    len: usize,
    duplicates: *const usize,
    duplicate_count: usize,
    root: usize,
    selector: *const PlsmSelector,
    slots: *mut BorrowedValue,
    capacity: usize,
    error: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            let input = unsafe { bytes(data, len) }?;
            let selector = unsafe { selector.as_ref() }.ok_or_else(usage)?;
            if capacity < selector.paths.len() || (!selector.paths.is_empty() && slots.is_null()) {
                return Err(usage());
            }
            let reader = Reader {
                doc: Lite3DocRef::new(input),
                duplicates: unsafe { offsets(duplicates, duplicate_count) }?,
            };
            for (index, path) in selector.paths.iter().enumerate() {
                let mut output = BorrowedValue::default();
                if let Some(offset) = reader.lookup(root, path)? {
                    output = reader.packed(offset)?;
                }
                unsafe {
                    *slots.add(index) = output;
                }
            }
            Ok(())
        })(),
        error,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_go_json(
    data: *const u8,
    len: usize,
    root: usize,
    out: *mut plsm_buf,
    error: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            let input = unsafe { bytes(data, len) }?;
            let doc = Lite3DocRef::new(input);
            if root >= len {
                return Err(usage());
            }
            let value = unsafe { JsonValue::from_validated(doc, root) };
            write_json_buf(out, serde_json::to_vec(&value).map_err(|_| corrupt())?)
        })(),
        error,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_go_message_json(
    data: *const u8,
    len: usize,
    duplicates: *const usize,
    duplicate_count: usize,
    data_node: usize,
    seq: u64,
    timestamp_ns: u64,
    out: *mut plsm_buf,
    error: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            let input = unsafe { bytes(data, len) }?;
            let reader = Reader {
                doc: Lite3DocRef::new(input),
                duplicates: unsafe { offsets(duplicates, duplicate_count) }?,
            };
            let meta = reader.get(0, "meta")?.ok_or_else(corrupt)?;
            let tags_node = reader.get(meta, "tags")?.ok_or_else(corrupt)?;
            let count = reader.doc.count_at(tags_node)?;
            let mut tags = Vec::with_capacity(count as usize);
            for index in 0..count {
                let mut offset = 0;
                if unsafe {
                    sys::plasmite_lite3_arr_get_val_ofs(
                        input.as_ptr(),
                        input.len(),
                        tags_node,
                        index,
                        &mut offset,
                    )
                } < 0
                {
                    return Err(corrupt());
                }
                let node = reader.node(offset)?;
                if node.kind != sys::LITE3_TYPE_STRING {
                    return Err(corrupt());
                }
                let bytes = unsafe { std::slice::from_raw_parts(node.text.cast(), node.text_len) };
                tags.push(unsafe { std::str::from_utf8_unchecked(bytes) });
            }
            let data = unsafe { JsonValue::from_validated(reader.doc, data_node) };
            let wire = crate::interface_wire::MessageWire::new(
                seq,
                crate::api::format_ts(timestamp_ns)?,
                &tags,
                data,
            );
            write_json_buf(out, serde_json::to_vec(&wire).map_err(|_| corrupt())?)
        })(),
        error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lite3::Lite3Buf;
    #[repr(C)]
    #[derive(Default)]
    struct Buffer {
        data: *mut u8,
        len: usize,
    }
    #[test]
    fn borrowed_validation_and_canonical_envelope_use_no_payload_copy() {
        let encoded =
            Lite3Buf::from_json_str(r#"{"meta":{"tags":[],"extra":7},"data":{"value":1}}"#)
                .unwrap();
        let mut root = 0;
        let mut meta = 0;
        let mut proof = Buffer::default();
        let mut error = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                plsm_go_validate(
                    encoded.as_slice().as_ptr(),
                    encoded.as_slice().len(),
                    &mut root,
                    &mut meta,
                    (&mut proof as *mut Buffer).cast(),
                    &mut error,
                )
            },
            0
        );
        assert_eq!(proof.len, 0);
        let mut out = Buffer::default();
        assert_eq!(
            unsafe {
                plsm_go_message_json(
                    encoded.as_slice().as_ptr(),
                    encoded.as_slice().len(),
                    std::ptr::null(),
                    0,
                    root,
                    u64::MAX,
                    1,
                    (&mut out as *mut Buffer).cast(),
                    &mut error,
                )
            },
            0
        );
        let json: serde_json::Value =
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(out.data, out.len) })
                .unwrap();
        assert_eq!(json["seq"].as_u64(), Some(u64::MAX));
        assert_eq!(json["meta"], serde_json::json!({"tags":[]}));
        assert_eq!(json["data"], serde_json::json!({"value":1}));
        crate::abi::plsm_buf_free((&mut out as *mut Buffer).cast());
    }
    #[test]
    fn duplicate_proof_keeps_lookup_count_and_json_consistent() {
        let encoded =
            Lite3Buf::from_json_str(r#"{"meta":{"tags":[]},"data":{"ab":1,"bA":2}}"#).unwrap();
        let data = encoded.as_doc().key_offset("data").unwrap();
        let second = encoded.as_doc().key_offset_at(data, "bA").unwrap();
        let mut changed = encoded.as_slice().to_vec();
        changed[second - 3..second - 1].copy_from_slice(b"ab");
        let mut root = 0;
        let mut meta = 0;
        let mut proof = Buffer::default();
        let mut error = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                plsm_go_validate(
                    changed.as_ptr(),
                    changed.len(),
                    &mut root,
                    &mut meta,
                    (&mut proof as *mut Buffer).cast(),
                    &mut error,
                )
            },
            0
        );
        assert_eq!(proof.len, size_of::<usize>());
        let raw = unsafe { std::slice::from_raw_parts(proof.data, proof.len) };
        let duplicates = raw
            .as_chunks::<{ size_of::<usize>() }>()
            .0
            .iter()
            .map(|bytes| usize::from_ne_bytes(*bytes))
            .collect::<Vec<_>>();
        let selector = PlsmSelector {
            paths: vec![vec!["ab".into()], vec![]],
        };
        let mut slots = [BorrowedValue::default(); 2];
        assert_eq!(
            unsafe {
                plsm_go_select(
                    changed.as_ptr(),
                    changed.len(),
                    duplicates.as_ptr(),
                    duplicates.len(),
                    root,
                    &selector,
                    slots.as_mut_ptr(),
                    slots.len(),
                    &mut error,
                )
            },
            0
        );
        assert_eq!(slots[0].integer, 2);
        assert_eq!(slots[1].len, 1);
        let mut json = Buffer::default();
        assert_eq!(
            unsafe {
                plsm_go_json(
                    changed.as_ptr(),
                    changed.len(),
                    root,
                    (&mut json as *mut Buffer).cast(),
                    &mut error,
                )
            },
            0
        );
        assert_eq!(
            unsafe { std::slice::from_raw_parts(json.data, json.len) },
            br#"{"ab":2}"#
        );
        crate::abi::plsm_buf_free((&mut json as *mut Buffer).cast());
        crate::abi::plsm_buf_free((&mut proof as *mut Buffer).cast());
    }
}
