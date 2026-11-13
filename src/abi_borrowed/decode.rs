//! Dispatch ordinary ASCII Go struct fields without formatting a whole object.
use super::{BorrowedValue, Reader, bytes, corrupt, finish, offsets, usage};
use crate::abi::plsm_error;
use crate::abi_views::PlsmSelector;
use crate::core::error::Error;
use crate::core::lite3::{Lite3DocRef, sys};
use std::collections::HashMap;

struct Node {
    name: Vec<u8>,
    object: bool,
    children: HashMap<Vec<u8>, usize>,
}
pub struct GoDecoder {
    nodes: Vec<Node>,
}
impl GoDecoder {
    fn new(paths: &[Vec<String>], objects: &[u8]) -> Result<Self, Error> {
        if paths.is_empty()
            || !paths[0].is_empty()
            || objects.len() != paths.len()
            || objects[0] != 1
        {
            return Err(usage());
        }
        let mut nodes = paths
            .iter()
            .enumerate()
            .map(|(i, path)| Node {
                name: path
                    .last()
                    .map(|name| name.as_bytes().to_vec())
                    .unwrap_or_default(),
                object: objects[i] != 0,
                children: HashMap::new(),
            })
            .collect::<Vec<_>>();
        for (index, path) in paths.iter().enumerate().skip(1) {
            let (key, prefix) = path.split_last().ok_or_else(usage)?;
            if !key.is_ascii() {
                return Err(usage());
            }
            let parent = paths[..index]
                .iter()
                .position(|p| p == prefix)
                .ok_or_else(usage)?;
            if !nodes[parent].object
                || nodes[parent]
                    .children
                    .insert(key.as_bytes().to_ascii_lowercase(), index)
                    .is_some()
            {
                return Err(usage());
            }
        }
        Ok(Self { nodes })
    }
    fn visit(
        &self,
        reader: &Reader<'_>,
        plan: usize,
        offset: usize,
        slots: &mut [BorrowedValue],
    ) -> Result<bool, Error> {
        if !self.nodes[plan].object {
            slots[plan] = reader.packed(offset)?;
            return Ok(true);
        }
        let value = reader.node(offset)?;
        if value.kind != sys::LITE3_TYPE_OBJECT {
            slots[plan] = reader.packed(offset)?;
            return Ok(true);
        }
        slots[plan] = BorrowedValue {
            kind: 7,
            node: offset,
            ..BorrowedValue::default()
        };
        let children = &self.nodes[plan].children;
        // The encoded object already stores its count. A complete exact shape
        // needs no scan for case aliases: every unique key matches one field.
        if reader.count(offset)? == children.len() {
            let mut complete = true;
            for &child in children.values() {
                let key = unsafe { std::str::from_utf8_unchecked(&self.nodes[child].name) };
                if let Some(node) = reader.get(offset, key)? {
                    slots[child] = BorrowedValue {
                        kind: 9,
                        node,
                        ..BorrowedValue::default()
                    };
                } else {
                    complete = false;
                    break;
                }
            }
            if complete {
                for &child in children.values() {
                    if !self.visit(reader, child, slots[child].node, slots)? {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
            for &child in children.values() {
                slots[child] = BorrowedValue::default();
            }
        }
        let mut state = [0u64; 32];
        if unsafe {
            sys::plasmite_lite3_cursor_init(
                reader.doc.bytes().as_ptr(),
                reader.doc.bytes().len(),
                offset,
                state.as_mut_ptr().cast(),
                size_of_val(&state),
            )
        } < 0
        {
            return Err(corrupt());
        }
        let mut lower = [0u8; 256];
        loop {
            let mut entry = sys::Entry::default();
            let mut hash = 0;
            let result = unsafe {
                sys::plasmite_lite3_cursor_next(
                    reader.doc.bytes().as_ptr(),
                    reader.doc.bytes().len(),
                    state.as_mut_ptr().cast(),
                    &mut entry,
                    &mut hash,
                )
            };
            if result < 0 {
                return Err(corrupt());
            }
            if result == 0 {
                break;
            }
            let key = unsafe { std::slice::from_raw_parts(entry.key.cast::<u8>(), entry.key_len) };
            let target = if key.len() <= lower.len() {
                for (i, &byte) in key.iter().enumerate() {
                    if !byte.is_ascii() {
                        return Ok(false);
                    }
                    lower[i] = byte.to_ascii_lowercase();
                }
                children.get(&lower[..key.len()]).copied()
            } else {
                if !key.is_ascii() {
                    return Ok(false);
                }
                children
                    .iter()
                    .find_map(|(name, &index)| name.eq_ignore_ascii_case(key).then_some(index))
            };
            let Some(target) = target else {
                continue;
            };
            let winner = &mut slots[target];
            // Canonical JSON drops equal duplicate keys. Distinct case aliases
            // can merge structs or preserve an earlier type error in Go, so
            // defer those uncommon records to the standard decoder.
            if winner.kind != 0 {
                let prior = &reader.doc.bytes()[winner.offset..winner.offset + winner.len];
                if key != prior {
                    return Ok(false);
                }
            }
            *winner = BorrowedValue {
                kind: 9,
                node: entry.offset,
                offset: entry.key as usize - reader.doc.bytes().as_ptr() as usize,
                len: key.len(),
                ..BorrowedValue::default()
            };
        }
        for &child in children.values() {
            if slots[child].kind != 0 && !self.visit(reader, child, slots[child].node, slots)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_go_decoder_new(
    selector: *const PlsmSelector,
    objects: *const u8,
    count: usize,
    out: *mut *mut GoDecoder,
    error: *mut *mut plsm_error,
) -> i32 {
    finish(
        (|| {
            let selector = unsafe { selector.as_ref() }.ok_or_else(usage)?;
            if out.is_null() || objects.is_null() || count != selector.paths.len() {
                return Err(usage());
            }
            let objects = unsafe { std::slice::from_raw_parts(objects, count) };
            let decoder = GoDecoder::new(&selector.paths, objects)?;
            unsafe {
                *out = Box::into_raw(Box::new(decoder));
            }
            Ok(())
        })(),
        error,
    )
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_go_decoder_free(decoder: *mut GoDecoder) {
    if !decoder.is_null() {
        drop(unsafe { Box::from_raw(decoder) });
    }
}
/// 1 requests the standard JSON path; it is a capability result, not an error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_go_decode_fields(
    data: *const u8,
    len: usize,
    duplicates: *const usize,
    count: usize,
    root: usize,
    decoder: *const GoDecoder,
    slots: *mut BorrowedValue,
    capacity: usize,
    error: *mut *mut plsm_error,
) -> i32 {
    let result = (|| {
        let input = unsafe { bytes(data, len) }?;
        let decoder = unsafe { decoder.as_ref() }.ok_or_else(usage)?;
        if slots.is_null() || capacity < decoder.nodes.len() {
            return Err(usage());
        }
        let slots = unsafe { std::slice::from_raw_parts_mut(slots, decoder.nodes.len()) };
        slots.fill(BorrowedValue::default());
        let reader = Reader {
            doc: Lite3DocRef::new(input),
            duplicates: unsafe { offsets(duplicates, count) }?,
        };
        decoder.visit(&reader, 0, root, slots)
    })();
    match result {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(error_value) => crate::abi::fail(error, error_value),
    }
}
