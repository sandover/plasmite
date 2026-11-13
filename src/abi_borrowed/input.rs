//! Pinned Go inputs borrow text and sealed source snapshots for one native call.
//! Mutable byte and JSON results must be private value-at-visit snapshots.
use super::{Reader, corrupt};
use crate::abi::{borrow_pool, fail, plsm_error, plsm_pool};
use crate::abi_views::{plsm_message_view, publish};
use crate::core::error::{Error, ErrorKind};
use crate::core::lite3::{Lite3DocRef, ParsedJson, builder::Builder, sys};
use crate::view::MessageView;

#[repr(C)]
pub struct Span {
    data: *const u8,
    len: usize,
}
#[repr(C)]
pub struct Node {
    parent: usize,
    kind: u32,
    flags: u32,
    integer: i64,
    real: f64,
    key: Span,
    text: Span,
}
fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage).with_message(message)
}
unsafe fn slice<'a, T>(data: *const T, len: usize) -> Result<&'a [T], Error> {
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() || len > isize::MAX as usize / size_of::<T>() {
        return Err(usage("invalid native input span"));
    }
    Ok(unsafe { std::slice::from_raw_parts(data, len) })
}
unsafe fn span<'a>(value: &Span) -> Result<&'a [u8], Error> {
    unsafe { slice(value.data, value.len) }
}
unsafe fn text<'a>(value: &Span) -> Result<&'a str, Error> {
    std::str::from_utf8(unsafe { span(value) }?).map_err(|_| usage("input text is not UTF-8"))
}
unsafe fn tags<'a>(values: &[Span]) -> Result<Vec<&'a str>, Error> {
    values.iter().map(|value| unsafe { text(value) }).collect()
}
fn cursor(reader: &Reader<'_>, offset: usize) -> Result<[u64; 32], Error> {
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
    Ok(state)
}
fn next(reader: &Reader<'_>, state: &mut [u64; 32]) -> Result<Option<sys::Entry>, Error> {
    let mut entry = sys::Entry::default();
    let mut hash = 0;
    match unsafe {
        sys::plasmite_lite3_cursor_next(
            reader.doc.bytes().as_ptr(),
            reader.doc.bytes().len(),
            state.as_mut_ptr().cast(),
            &mut entry,
            &mut hash,
        )
    } {
        n if n < 0 => Err(corrupt()),
        0 => Ok(None),
        _ => Ok(Some(entry)),
    }
}
fn entry_key(entry: &sys::Entry) -> Option<&str> {
    (!entry.key.is_null()).then(|| unsafe {
        std::str::from_utf8_unchecked(std::slice::from_raw_parts(entry.key.cast(), entry.key_len))
    })
}
fn copy_children(
    builder: &mut Builder,
    parent: usize,
    reader: &Reader<'_>,
    source: usize,
    depth: usize,
) -> Result<(), Error> {
    let mut state = cursor(reader, source)?;
    while let Some(entry) = next(reader, &mut state)? {
        copy_source(
            builder,
            parent,
            entry_key(&entry),
            reader,
            entry.offset,
            depth,
        )?
    }
    Ok(())
}
fn copy_source(
    builder: &mut Builder,
    parent: usize,
    key: Option<&str>,
    reader: &Reader<'_>,
    source: usize,
    depth: usize,
) -> Result<(), Error> {
    let node = reader.node(source)?;
    let container = matches!(node.kind, sys::LITE3_TYPE_OBJECT | sys::LITE3_TYPE_ARRAY);
    if container && depth >= 32 {
        return Err(usage("container nesting exceeds limit"));
    }
    // Source bytes already passed whole-envelope validation and remain immutable.
    // Setters copy spans now; duplicate names overwrite in native iteration order.
    let target = builder.put(parent, key, node)?;
    if container {
        copy_children(builder, target, reader, source, depth + 1)?
    }
    Ok(())
}
unsafe fn source_reader<'a>(value: &Span) -> Result<Reader<'a>, Error> {
    let input = unsafe { super::bytes(value.data, value.len) }?;
    Ok(Reader {
        doc: Lite3DocRef::new(input),
        duplicates: &[],
    })
}
unsafe fn build(nodes: &[Node], tag_spans: &[Span]) -> Result<MessageView, Error> {
    let root = nodes.first().ok_or_else(|| usage("data must be object"))?;
    if root.parent != usize::MAX || root.flags >> 8 != 1 {
        return Err(usage("invalid native root"));
    }
    let tags = unsafe { tags(tag_spans) }?;
    if root.kind == 9 {
        if nodes.len() != 1 {
            return Err(usage("invalid JSON root children"));
        }
        return ParsedJson::parse_view(unsafe { span(&root.text) }?)?.encode_message_view(&tags);
    }
    let mut builder = Builder::new(&tags)?;
    if root.kind == 10 {
        if nodes.len() != 1 {
            return Err(usage("invalid source root children"));
        }
        let reader = unsafe { source_reader(&root.text) }?;
        let offset = usize::try_from(root.integer).map_err(|_| usage("invalid source node"))?;
        if reader.node(offset)?.kind != sys::LITE3_TYPE_OBJECT {
            return Err(usage("data must be object"));
        }
        let parent = builder.data;
        copy_children(&mut builder, parent, &reader, offset, 2)?;
    } else {
        if root.kind != 7 {
            return Err(usage("data must be object"));
        }
        let mut offsets = Vec::with_capacity(nodes.len());
        offsets.push(builder.data);
        for (index, node) in nodes.iter().enumerate().skip(1) {
            if node.parent >= index || !matches!(nodes[node.parent].kind, 7 | 8) {
                return Err(usage("invalid native parent"));
            }
            let depth = (node.flags >> 8) as usize;
            if depth != (nodes[node.parent].flags >> 8) as usize + 1 {
                return Err(usage("invalid native depth"));
            }
            let keyed = node.flags & 1 != 0;
            if keyed != (nodes[node.parent].kind == 7) {
                return Err(usage("invalid native key"));
            }
            let key = if keyed {
                Some(unsafe { text(&node.key) }?)
            } else {
                None
            };
            let parent = offsets[node.parent];
            if node.kind == 9 {
                let parsed = ParsedJson::parse_view(unsafe { span(&node.text) }?)?;
                crate::view::copy_serialized(&mut builder, parent, key, &parsed.value(), depth)?;
                offsets.push(usize::MAX);
                continue;
            }
            if node.kind == 10 {
                let reader = unsafe { source_reader(&node.text) }?;
                let source =
                    usize::try_from(node.integer).map_err(|_| usage("invalid source node"))?;
                copy_source(&mut builder, parent, key, &reader, source, depth)?;
                offsets.push(usize::MAX);
                continue;
            }
            let kind = match node.kind {
                1 => sys::LITE3_TYPE_NULL,
                2 => sys::LITE3_TYPE_BOOL,
                3 => sys::LITE3_TYPE_I64,
                4 => sys::LITE3_TYPE_F64,
                5 => sys::LITE3_TYPE_STRING,
                6 => sys::LITE3_TYPE_BYTES,
                7 => sys::LITE3_TYPE_OBJECT,
                8 => sys::LITE3_TYPE_ARRAY,
                _ => return Err(usage("invalid native input kind")),
            };
            if matches!(kind, sys::LITE3_TYPE_OBJECT | sys::LITE3_TYPE_ARRAY) && depth >= 32 {
                return Err(usage("container nesting exceeds limit"));
            }
            if kind == sys::LITE3_TYPE_F64 && !node.real.is_finite() {
                return Err(usage("number must be finite"));
            }
            let bytes = if kind == sys::LITE3_TYPE_STRING {
                unsafe { text(&node.text) }?.as_bytes()
            } else if kind == sys::LITE3_TYPE_BYTES {
                unsafe { span(&node.text) }?
            } else {
                &[]
            };
            let target = builder.put(
                parent,
                key,
                sys::ValueView {
                    kind,
                    integer: node.integer,
                    real: node.real,
                    text: bytes.as_ptr().cast(),
                    text_len: bytes.len(),
                },
            )?;
            offsets.push(target);
        }
    }
    // Native setters establish structure and key indexes. Root shape is fixed;
    // UTF-8/finite/depth checks above also cover JSON and checked source copies.
    // Every mutable source was snapshotted before this call, and setters copy.
    unsafe { builder.finish_proven() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn plsm_go_append_nodes(
    pool: *mut plsm_pool,
    nodes: *const Node,
    count: usize,
    tag_spans: *const Span,
    tag_count: usize,
    durability: u32,
    out: *mut *const plsm_message_view,
    error: *mut *mut plsm_error,
) -> i32 {
    if out.is_null() {
        return fail(error, usage("null append result"));
    }
    let pool = match borrow_pool(pool, error) {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    super::finish(
        (|| {
            let durability = match durability {
                0 => crate::api::Durability::Fast,
                1 => crate::api::Durability::Flush,
                _ => return Err(usage("invalid durability")),
            };
            let mut view = unsafe { build(slice(nodes, count)?, slice(tag_spans, tag_count)?) }?;
            let time = crate::api::now_ns()?;
            let seq = pool.pool.append_with_options(
                view.bytes(),
                crate::api::AppendOptions::new(time, durability),
            )?;
            view.seq = seq;
            view.timestamp_ns = time;
            publish(view, out)
        })(),
        error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text_span(text: &str) -> Span {
        Span {
            data: text.as_ptr(),
            len: text.len(),
        }
    }
    fn empty_span() -> Span {
        Span {
            data: std::ptr::null(),
            len: 0,
        }
    }
    fn root() -> Node {
        Node {
            parent: usize::MAX,
            kind: 7,
            flags: 1 << 8,
            integer: 0,
            real: 0.0,
            key: empty_span(),
            text: empty_span(),
        }
    }
    #[test]
    fn pinned_input_proves_scalar_text_and_parent_depth() {
        let mut nodes = [
            root(),
            Node {
                parent: 0,
                kind: 4,
                flags: (2 << 8) | 1,
                integer: 0,
                real: 1e20,
                key: text_span("value"),
                text: empty_span(),
            },
        ];
        let view = unsafe { build(&nodes, &[]) }.unwrap();
        assert_eq!(
            view.data().get("value").unwrap().unwrap().real(),
            Some(1e20)
        );
        nodes[1].flags = (3 << 8) | 1;
        assert!(unsafe { build(&nodes, &[]) }.is_err());
        nodes[1].flags = (2 << 8) | 1;
        nodes[1].real = f64::INFINITY;
        assert!(unsafe { build(&nodes, &[]) }.is_err());
        nodes[1].real = 0.0;
        nodes[1].kind = 5;
        nodes[1].text = Span {
            data: b"\xff".as_ptr(),
            len: 1,
        };
        assert!(unsafe { build(&nodes, &[]) }.is_err());
        nodes[1].text = text_span("a\0b");
        assert!(unsafe { build(&nodes, &[]) }.is_ok());
        nodes[1].key = text_span("bad\0key");
        assert!(unsafe { build(&nodes, &[]) }.is_err());
    }
}
