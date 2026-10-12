//! Read one complete CLI message at a time and write framed Lite3 messages.

use clap::ValueEnum;
use plasmite::api::{Error, ErrorKind, lite3};
use serde_json::{Map, Number, Value};
use serde_yaml_ng::Value as YamlValue;
use std::io::{self, BufRead, BufReader, Read, Write};

pub(crate) const LITE3_STREAM_MAGIC: &[u8; 4] = b"PLS1";
const MAX_PAYLOAD_BYTES: usize = lite3::MAX_LITE3_BUF;
// A control byte can take six JSON bytes ("\\u00xx"). Leave room for the envelope.
const MAX_TEXT_RECORD_BYTES: usize = 6 * MAX_PAYLOAD_BYTES + 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum InputFormat {
    Auto,
    Json,
    Yaml,
    Lite3,
    Lite3Raw,
}

#[derive(Debug)]
pub(crate) enum InputRecord {
    Json(Value),
    Lite3 {
        seq: u64,
        timestamp_ns: u64,
        payload: Vec<u8>,
    },
}

pub(crate) fn read_records<R, F>(
    reader: R,
    format: InputFormat,
    mut on_record: F,
) -> Result<u64, Error>
where
    R: Read,
    F: FnMut(InputRecord) -> Result<(), Error>,
{
    let mut reader = BufReader::new(reader);
    match format {
        InputFormat::Auto => {
            let Some(first) = next_non_whitespace(&mut reader)? else {
                return Ok(0);
            };
            match first {
                b'{' => read_json_records(&mut reader, Some(first), &mut on_record),
                b'-' => {
                    let mut rest = [0; 2];
                    read_required(&mut reader, &mut rest, "incomplete YAML document marker")?;
                    if rest != *b"--" {
                        return Err(input_error("unknown input format"));
                    }
                    let prefix = io::Cursor::new(b"---".to_vec());
                    read_yaml_records(BufReader::new(prefix.chain(reader)), &mut on_record)
                }
                b'P' => {
                    let mut rest = [0; 3];
                    read_required(&mut reader, &mut rest, "incomplete Lite3 stream prefix")?;
                    if rest != LITE3_STREAM_MAGIC[1..] {
                        return Err(input_error("unknown input format"));
                    }
                    read_lite3_records(&mut reader, true, &mut on_record)
                }
                _ => Err(input_error("unknown input format")),
            }
        }
        InputFormat::Json => read_json_records(&mut reader, None, &mut on_record),
        InputFormat::Yaml => read_yaml_records(reader, &mut on_record),
        InputFormat::Lite3 => read_lite3_records(&mut reader, false, &mut on_record),
        InputFormat::Lite3Raw => read_raw_lite3(&mut reader, &mut on_record),
    }
}

fn read_json_records<R, F>(
    reader: &mut R,
    first: Option<u8>,
    on_record: &mut F,
) -> Result<u64, Error>
where
    R: Read,
    F: FnMut(InputRecord) -> Result<(), Error>,
{
    let mut next = first;
    let mut count = 0;
    loop {
        let start = match next.take() {
            Some(byte) => Some(byte),
            None => next_non_whitespace(reader)?,
        };
        let Some(start) = start else { return Ok(count) };
        if start != b'{' {
            return Err(input_error("JSON input requires message objects"));
        }
        let mut record = vec![start];
        let mut depth = 1usize;
        let mut in_string = false;
        let mut escaped = false;
        while depth != 0 {
            let Some(byte) = read_byte(reader)? else {
                return Err(input_error("incomplete JSON message"));
            };
            push_bounded(&mut record, byte)?;
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => depth -= 1,
                _ => {}
            }
        }
        let value = serde_json::from_slice(&record)
            .map_err(|err| input_error("invalid JSON message").with_source(err))?;
        on_record(InputRecord::Json(value))?;
        count += 1;
    }
}

fn read_yaml_records<R, F>(mut reader: R, on_record: &mut F) -> Result<u64, Error>
where
    R: BufRead,
    F: FnMut(InputRecord) -> Result<(), Error>,
{
    let mut document = Vec::new();
    let mut line = Vec::new();
    let mut marked = false;
    let mut finished_marked = false;
    let mut count = 0;
    loop {
        line.clear();
        let read = read_line_bounded(
            &mut reader,
            &mut line,
            MAX_TEXT_RECORD_BYTES.saturating_sub(document.len()),
        )?;
        if read == 0 {
            if marked {
                return Err(input_error("YAML document is missing its ... end marker"));
            }
            if !document.is_empty() {
                on_record(InputRecord::Json(parse_yaml_document(&document)?))?;
                count += 1;
            }
            return Ok(count);
        }
        // YAML document markers start in column zero. Indented "..." can be
        // ordinary text inside a block scalar.
        let marker = line.trim_ascii_end();
        if document.is_empty() {
            if line.trim_ascii().is_empty() {
                continue;
            }
            if marker == b"---" {
                marked = true;
                append_bounded(&mut document, &line)?;
                continue;
            }
            if finished_marked {
                return Err(input_error("YAML messages require a --- start marker"));
            }
            if marker == b"..." {
                return Err(input_error("unexpected YAML end marker"));
            }
            append_bounded(&mut document, &line)?;
            continue;
        }
        if marked && marker == b"..." {
            append_bounded(&mut document, &line)?;
            on_record(InputRecord::Json(parse_yaml_document(&document)?))?;
            count += 1;
            document.clear();
            marked = false;
            finished_marked = true;
            continue;
        }
        if marker == b"---" || marker == b"..." {
            return Err(input_error(
                "YAML messages require explicit document boundaries",
            ));
        }
        append_bounded(&mut document, &line)?;
    }
}

fn parse_yaml_document(bytes: &[u8]) -> Result<Value, Error> {
    let mut budget = serde_saphyr::Budget::default();
    budget.max_aliases = 0;
    budget.max_depth = 128;
    budget.max_documents = 1;
    budget.max_inclusion_depth = 0;
    budget.max_total_scalar_bytes = MAX_TEXT_RECORD_BYTES;
    budget.max_total_comment_bytes = MAX_TEXT_RECORD_BYTES;
    budget.max_events = MAX_TEXT_RECORD_BYTES;
    budget.max_nodes = MAX_TEXT_RECORD_BYTES;
    let mut options = serde_saphyr::Options::default();
    options.budget = Some(budget);
    options.merge_keys = serde_saphyr::options::MergeKeyPolicy::Error;
    // This pass rejects aliases and duplicate keys before the value parser expands them.
    let _: Value = serde_saphyr::from_slice_with_options(bytes, options)
        .map_err(|err| input_error("invalid or unsupported YAML message").with_source(err))?;
    // This parser retains tagged values and exact numeric types that the budget pass discards.
    let yaml: YamlValue = serde_yaml_ng::from_slice(bytes)
        .map_err(|err| input_error("invalid YAML message").with_source(err))?;
    yaml_to_json(yaml)
}

fn yaml_to_json(value: YamlValue) -> Result<Value, Error> {
    Ok(match value {
        YamlValue::Null => Value::Null,
        YamlValue::Bool(value) => Value::Bool(value),
        YamlValue::String(value) => Value::String(value),
        YamlValue::Number(value) if value.is_f64() => {
            let number = Number::from_f64(value.as_f64().expect("float"))
                .ok_or_else(|| input_error("YAML contains a non-finite number"))?;
            Value::Number(number)
        }
        YamlValue::Number(value) => {
            let number = value
                .as_i64()
                .map(Number::from)
                .or_else(|| value.as_u64().map(Number::from))
                .ok_or_else(|| input_error("YAML number is outside JSON's range"))?;
            Value::Number(number)
        }
        YamlValue::Sequence(values) => Value::Array(
            values
                .into_iter()
                .map(yaml_to_json)
                .collect::<Result<_, _>>()?,
        ),
        YamlValue::Mapping(values) => {
            let mut map = Map::new();
            for (key, value) in values {
                let YamlValue::String(key) = key else {
                    return Err(input_error("YAML object keys must be strings"));
                };
                map.insert(key, yaml_to_json(value)?);
            }
            Value::Object(map)
        }
        YamlValue::Tagged(_) => return Err(input_error("YAML tags are not supported")),
    })
}

fn read_lite3_records<R, F>(
    reader: &mut R,
    prefix_consumed: bool,
    on_record: &mut F,
) -> Result<u64, Error>
where
    R: Read,
    F: FnMut(InputRecord) -> Result<(), Error>,
{
    if !prefix_consumed {
        let mut magic = [0; 4];
        if !read_exact_or_eof(reader, &mut magic)? {
            return Ok(0);
        }
        if magic != *LITE3_STREAM_MAGIC {
            return Err(input_error("invalid Lite3 stream prefix"));
        }
    }
    let mut count = 0;
    loop {
        let mut header = [0; 20];
        if !read_exact_or_eof(reader, &mut header)? {
            return Ok(count);
        }
        let seq = u64::from_be_bytes(header[..8].try_into().expect("fixed header"));
        let timestamp_ns = u64::from_be_bytes(header[8..16].try_into().expect("fixed header"));
        let len = u32::from_be_bytes(header[16..].try_into().expect("fixed header")) as usize;
        if len > MAX_PAYLOAD_BYTES {
            return Err(input_error("Lite3 payload exceeds 256 MiB"));
        }
        let mut payload = vec![0; len];
        read_required(reader, &mut payload, "incomplete Lite3 payload")?;
        lite3::validate_bytes(&payload)?;
        on_record(InputRecord::Lite3 {
            seq,
            timestamp_ns,
            payload,
        })?;
        count += 1;
    }
}

fn read_raw_lite3<R, F>(reader: &mut R, on_record: &mut F) -> Result<u64, Error>
where
    R: Read,
    F: FnMut(InputRecord) -> Result<(), Error>,
{
    let mut payload = Vec::new();
    reader
        .take((MAX_PAYLOAD_BYTES + 1) as u64)
        .read_to_end(&mut payload)
        .map_err(read_error)?;
    if payload.is_empty() {
        return Ok(0);
    }
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(input_error("Lite3 payload exceeds 256 MiB"));
    }
    lite3::validate_bytes(&payload)?;
    on_record(InputRecord::Lite3 {
        seq: 0,
        timestamp_ns: 0,
        payload,
    })?;
    Ok(1)
}

pub(crate) fn write_header(writer: &mut impl Write) -> Result<(), Error> {
    writer
        .write_all(LITE3_STREAM_MAGIC)
        .and_then(|()| writer.flush())
        .map_err(write_error)
}

pub(crate) fn write_frame(
    writer: &mut impl Write,
    seq: u64,
    timestamp_ns: u64,
    payload: &[u8],
) -> Result<(), Error> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(input_error("Lite3 payload exceeds 256 MiB"));
    }
    let len = u32::try_from(payload.len()).expect("256 MiB fits u32");
    writer
        .write_all(&seq.to_be_bytes())
        .and_then(|()| writer.write_all(&timestamp_ns.to_be_bytes()))
        .and_then(|()| writer.write_all(&len.to_be_bytes()))
        .and_then(|()| writer.write_all(payload))
        .and_then(|()| writer.flush())
        .map_err(write_error)
}

pub(crate) fn write_raw(writer: &mut impl Write, payload: &[u8]) -> Result<(), Error> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(input_error("Lite3 payload exceeds 256 MiB"));
    }
    writer
        .write_all(payload)
        .and_then(|()| writer.flush())
        .map_err(write_error)
}

fn next_non_whitespace(reader: &mut impl Read) -> Result<Option<u8>, Error> {
    loop {
        match read_byte(reader)? {
            Some(byte) if byte.is_ascii_whitespace() => continue,
            other => return Ok(other),
        }
    }
}

fn read_byte(reader: &mut impl Read) -> Result<Option<u8>, Error> {
    let mut byte = [0];
    match reader.read(&mut byte).map_err(read_error)? {
        0 => Ok(None),
        _ => Ok(Some(byte[0])),
    }
}

fn read_exact_or_eof(reader: &mut impl Read, bytes: &mut [u8]) -> Result<bool, Error> {
    let Some(first) = read_byte(reader)? else {
        return Ok(false);
    };
    bytes[0] = first;
    read_required(reader, &mut bytes[1..], "incomplete Lite3 frame")?;
    Ok(true)
}

fn read_required(reader: &mut impl Read, bytes: &mut [u8], message: &str) -> Result<(), Error> {
    reader
        .read_exact(bytes)
        .map_err(|err| input_error(message).with_source(err))
}

fn read_line_bounded(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    limit: usize,
) -> Result<usize, Error> {
    loop {
        let available = reader.fill_buf().map_err(read_error)?;
        if available.is_empty() {
            return Ok(line.len());
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |n| n + 1);
        if line.len() + take > limit {
            return Err(input_error("text message exceeds the record limit"));
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if line.last() == Some(&b'\n') {
            return Ok(line.len());
        }
    }
}

fn push_bounded(record: &mut Vec<u8>, byte: u8) -> Result<(), Error> {
    if record.len() >= MAX_TEXT_RECORD_BYTES {
        return Err(input_error("text message exceeds the record limit"));
    }
    record.push(byte);
    Ok(())
}

fn append_bounded(record: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    if record.len() + bytes.len() > MAX_TEXT_RECORD_BYTES {
        return Err(input_error("text message exceeds the record limit"));
    }
    record.extend_from_slice(bytes);
    Ok(())
}

fn input_error(message: &str) -> Error {
    Error::new(ErrorKind::Usage).with_message(message)
}

fn read_error(err: io::Error) -> Error {
    Error::new(ErrorKind::Io)
        .with_message("failed to read message input")
        .with_source(err)
}

fn write_error(err: io::Error) -> Error {
    Error::new(ErrorKind::Io)
        .with_message("failed to write message output")
        .with_source(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn json_auto_delivers_each_object_before_reading_the_next() {
        struct FirstOnly<'a>(&'a [u8]);
        impl Read for FirstOnly<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0.is_empty() {
                    return Err(io::Error::other("reader polled after first object"));
                }
                let n = self.0.len().min(buf.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let err = read_records(
            FirstOnly(br#"{"data":{"heartbeat":true}}"#),
            InputFormat::Auto,
            |record| {
                assert!(matches!(record, InputRecord::Json(_)));
                Err(input_error("callback reached"))
            },
        )
        .expect_err("callback must run before another read");
        assert_eq!(err.message(), Some("callback reached"));
    }

    #[test]
    fn json_stream_accepts_pretty_objects_and_stops_on_later_error() {
        let mut seen = Vec::new();
        let err = read_records(
            Cursor::new(b"{\n\"data\": {\"x\": \"}\\\"\"}\n}\n{\"data\": {\"x\": 2}}\n{bad"),
            InputFormat::Json,
            |record| {
                let InputRecord::Json(value) = record else {
                    panic!("JSON")
                };
                seen.push(value);
                Ok(())
            },
        )
        .expect_err("third record is invalid");
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1]["data"]["x"], 2);
        assert_eq!(err.message(), Some("incomplete JSON message"));
    }

    #[test]
    fn yaml_preserves_json_types_and_rejects_unsupported_values() {
        let mut seen = Vec::new();
        let input = b"---\ndata:\n  count: 18446744073709551615\n  ratio: 1.0\n  text: '2026-10-12'\n...\n---\ndata:\n  wrong: !Thing 2\n...\n";
        let err = read_records(Cursor::new(input), InputFormat::Auto, |record| {
            let InputRecord::Json(value) = record else {
                panic!("YAML")
            };
            seen.push(value);
            Ok(())
        })
        .expect_err("custom tag rejected");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["data"]["count"].as_u64(), Some(u64::MAX));
        assert!(seen[0]["data"]["ratio"].is_f64());
        assert_eq!(seen[0]["data"]["text"], "2026-10-12");
        assert_eq!(err.message(), Some("YAML tags are not supported"));
        for input in [
            b"---\ndata: {x: .nan}\n...\n".as_slice(),
            b"---\ndata: {1: hi}\n...\n",
            b"---\na: &x [1]\nb: *x\n...\n",
        ] {
            assert!(read_records(Cursor::new(input), InputFormat::Yaml, |_| Ok(())).is_err());
        }
    }

    #[test]
    fn yaml_marker_text_inside_block_scalar_stays_in_the_message() {
        let input = b"---\ndata:\n  text: |\n    ...\n    ---\n...\n";
        let mut seen = Vec::new();
        assert_eq!(
            read_records(Cursor::new(input), InputFormat::Yaml, |record| {
                let InputRecord::Json(value) = record else {
                    panic!("YAML")
                };
                seen.push(value);
                Ok(())
            })
            .unwrap(),
            1
        );
        assert_eq!(seen[0]["data"]["text"], "...\n---\n");
    }

    #[test]
    fn explicit_yaml_accepts_one_unmarked_document_at_eof() {
        let mut seen = Vec::new();
        assert_eq!(
            read_records(
                Cursor::new(b"data: {ready: true}\n"),
                InputFormat::Yaml,
                |record| {
                    let InputRecord::Json(value) = record else {
                        panic!("YAML")
                    };
                    seen.push(value);
                    Ok(())
                }
            )
            .unwrap(),
            1
        );
        assert_eq!(seen[0]["data"]["ready"], true);
        assert_eq!(
            read_records(Cursor::new(b"   \n"), InputFormat::Auto, |_| Ok(())).unwrap(),
            0
        );
    }

    #[test]
    fn lite3_frames_round_trip_and_truncation_keeps_previous_record() {
        let payload = lite3::encode_message(&["tag".into()], &json!({"x": 1})).unwrap();
        let mut bytes = Vec::new();
        write_header(&mut bytes).unwrap();
        write_frame(&mut bytes, 7, 9, payload.as_slice()).unwrap();
        write_frame(&mut bytes, 8, 10, payload.as_slice()).unwrap();
        let mut seen = Vec::new();
        assert_eq!(
            read_records(Cursor::new(&bytes), InputFormat::Auto, |record| {
                let InputRecord::Lite3 {
                    seq,
                    timestamp_ns,
                    payload: found,
                } = record
                else {
                    panic!("Lite3")
                };
                seen.push((seq, timestamp_ns, found));
                Ok(())
            })
            .unwrap(),
            2
        );
        assert_eq!(seen[0], (7, 9, payload.as_slice().to_vec()));
        for cut in [bytes.len() - 1, bytes.len() - payload.len() - 1] {
            let mut accepted = 0;
            assert!(
                read_records(Cursor::new(&bytes[..cut]), InputFormat::Lite3, |_| {
                    accepted += 1;
                    Ok(())
                })
                .is_err()
            );
            assert_eq!(accepted, 1);
        }
        assert_eq!(
            read_records(Cursor::new(LITE3_STREAM_MAGIC), InputFormat::Lite3, |_| Ok(
                ()
            ))
            .unwrap(),
            0
        );
    }

    #[test]
    fn raw_lite3_requires_explicit_format() {
        let payload = lite3::encode_message(&[], &json!({"x": 1})).unwrap();
        assert!(
            read_records(Cursor::new(payload.as_slice()), InputFormat::Auto, |_| Ok(
                ()
            ))
            .is_err()
        );
        assert_eq!(
            read_records(
                Cursor::new(payload.as_slice()),
                InputFormat::Lite3Raw,
                |_| Ok(())
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn bounded_line_reader_stops_at_limit() {
        let mut reader = BufReader::new(Cursor::new(b"12345\n"));
        let mut line = Vec::new();
        let err = read_line_bounded(&mut reader, &mut line, 5).expect_err("limit");
        assert_eq!(err.message(), Some("text message exceeds the record limit"));
    }

    #[test]
    fn binary_writer_preserves_broken_pipe_source() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let err = write_header(&mut Closed).expect_err("closed output");
        let source = std::error::Error::source(&err)
            .and_then(|source| source.downcast_ref::<io::Error>())
            .expect("I/O source");
        assert_eq!(source.kind(), io::ErrorKind::BrokenPipe);
    }
}
