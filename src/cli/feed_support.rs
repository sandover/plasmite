//! Append complete CLI messages through the same local and remote API paths.

use super::message_stream::{InputFormat, InputRecord, read_records};
use super::output::OutputMode;
use super::output_support::{emit_lite3, emit_message};
use super::support::{RetryConfig, message_to_json, now_ns, retry_append};
use crate::ColorMode;
use plasmite::api::{
    AppendOptions, Durability, Error, ErrorKind, Message, Pool, PoolApiExt, RemotePool, lite3,
};
use serde::Deserialize;
use serde_json::Value;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageInput {
    seq: u64,
    time: String,
    meta: MessageMeta,
    data: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageMeta {
    tags: Vec<String>,
}

// Reserve only the canonical output envelope. Partial envelopes and objects
// with application fields remain ordinary data, even when they contain `data`.
fn is_message_shape(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == 4
            && ["seq", "time", "meta", "data"]
                .iter()
                .all(|key| object.contains_key(*key))
            && object["meta"]
                .as_object()
                .is_some_and(|meta| meta.len() == 1 && meta.contains_key("tags"))
    })
}

pub(crate) fn record_payload(record: InputRecord, tags: &[String]) -> Result<Vec<u8>, Error> {
    match record {
        InputRecord::Lite3 {
            seq,
            timestamp_ns,
            payload,
        } => {
            let _ = (seq, timestamp_ns);
            if !tags.is_empty() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("Lite3 already contains a complete message")
                    .with_hint("Remove --tag for Lite3 input."));
            }
            Ok(payload)
        }
        InputRecord::Json(value) => {
            let (data, tags) = if !is_message_shape(&value) {
                (value, tags.to_vec())
            } else {
                if !tags.is_empty() {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("complete messages supply their own tags")
                        .with_hint("Remove --tag to preserve the message's original tags."));
                }
                let message: MessageInput = serde_json::from_value(value).map_err(|err| {
                    Error::new(ErrorKind::Usage)
                        .with_message("invalid complete message")
                        .with_hint(
                            "Expected seq, RFC 3339 time, meta.tags strings, and object data.",
                        )
                        .with_source(err)
                })?;
                // Identity belongs to the source pool. The append assigns its own identity.
                let _ = message.seq;
                time::OffsetDateTime::parse(
                    &message.time,
                    &time::format_description::well_known::Rfc3339,
                )
                .map_err(|err| {
                    Error::new(ErrorKind::Usage)
                        .with_message("invalid source message time")
                        .with_hint("Use an RFC 3339 timestamp.")
                        .with_source(err)
                })?;
                (message.data, message.meta.tags)
            };
            Ok(lite3::encode_message(&tags, &data)
                .map_err(|err| err.with_hint("Put message contents in an object."))?
                .as_slice()
                .to_vec())
        }
    }
}

pub(crate) fn missing_feed_data_error() -> Error {
    Error::new(ErrorKind::Usage)
        .with_message("missing message input")
        .with_hint("Pipe messages to feed, use --file, or provide a message argument.")
}

pub(crate) fn open_feed_reader(path: &str) -> Result<Box<dyn Read>, Error> {
    if path == "-" {
        return Ok(Box::new(io::stdin()));
    }
    Ok(Box::new(std::fs::File::open(path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read message file")
            .with_path(path)
            .with_source(err)
    })?))
}

pub(crate) struct FeedIngestContext<'a> {
    pub(crate) tags: &'a [String],
    pub(crate) durability: Durability,
    pub(crate) retry_config: Option<RetryConfig>,
    pub(crate) pool_handle: &'a mut Pool,
    pub(crate) color_mode: ColorMode,
    pub(crate) output_mode: OutputMode,
    pub(crate) input: InputFormat,
    pub(crate) stop: Option<&'a AtomicBool>,
}

pub(crate) struct RemoteFeedIngestContext<'a> {
    pub(crate) tags: &'a [String],
    pub(crate) durability: Durability,
    pub(crate) retry_config: Option<RetryConfig>,
    pub(crate) remote_pool: &'a RemotePool,
    pub(crate) color_mode: ColorMode,
    pub(crate) output_mode: OutputMode,
    pub(crate) input: InputFormat,
    pub(crate) stop: Option<&'a AtomicBool>,
}

#[derive(Copy, Clone)]
struct IngestOptions<'a> {
    tags: &'a [String],
    input: InputFormat,
    output: OutputMode,
    color: ColorMode,
    emit: bool,
    stop: Option<&'a AtomicBool>,
}

enum Receipt {
    Text(Message),
    Binary { seq: u64, timestamp_ns: u64 },
}

fn ingest_with_append(
    reader: impl Read,
    options: IngestOptions<'_>,
    mut append: impl FnMut(&[u8]) -> Result<Receipt, Error>,
) -> Result<u64, Error> {
    let count = read_records(reader, options.input, |record| {
        let payload = record_payload(record, options.tags)?;
        if options
            .stop
            .is_some_and(|stop| stop.load(Ordering::Acquire))
        {
            return Ok(());
        }
        let receipt = append(&payload)?;
        // A stdout error stops ingestion after the append has succeeded.
        if options.emit {
            match receipt {
                Receipt::Text(message) => {
                    emit_message(message_to_json(message), options.output, options.color)?;
                }
                Receipt::Binary { seq, timestamp_ns } => {
                    emit_lite3(seq, timestamp_ns, &payload, options.output)?;
                }
            }
        }
        Ok(())
    })?;
    Ok(count)
}

pub(crate) fn ingest_from_stdin(
    reader: impl Read,
    ctx: FeedIngestContext<'_>,
    emit_receipt: bool,
) -> Result<u64, Error> {
    let options = IngestOptions {
        tags: ctx.tags,
        input: ctx.input,
        output: ctx.output_mode,
        color: ctx.color_mode,
        emit: emit_receipt,
        stop: ctx.stop,
    };
    ingest_with_append(reader, options, |payload| {
        retry_append(ctx.retry_config, || {
            if options.output.is_binary() {
                let timestamp_ns = now_ns()?;
                let seq = ctx
                    .pool_handle
                    .append_lite3(payload, AppendOptions::new(timestamp_ns, ctx.durability))?;
                Ok(Receipt::Binary { seq, timestamp_ns })
            } else {
                ctx.pool_handle
                    .append_lite3_with_receipt_now(payload, ctx.durability)
                    .map(Receipt::Text)
            }
        })
    })
}

pub(crate) fn ingest_from_stdin_remote(
    reader: impl Read,
    ctx: RemoteFeedIngestContext<'_>,
    emit_receipt: bool,
) -> Result<u64, Error> {
    let options = IngestOptions {
        tags: ctx.tags,
        input: ctx.input,
        output: ctx.output_mode,
        color: ctx.color_mode,
        emit: emit_receipt,
        stop: ctx.stop,
    };
    ingest_with_append(reader, options, |payload| {
        retry_append(ctx.retry_config, || {
            let append = AppendOptions::new(0, ctx.durability);
            if options.output.is_binary() {
                let (seq, timestamp_ns) = ctx.remote_pool.append_lite3_identity(payload, append)?;
                Ok(Receipt::Binary { seq, timestamp_ns })
            } else {
                ctx.remote_pool
                    .append_lite3_with_receipt(payload, append)
                    .map(Receipt::Text)
            }
        })
    })
}

pub(crate) fn ingest_from_stdin_udp(
    reader: impl Read,
    pool: &plasmite::api::UdpPool,
    tags: &[String],
    input: InputFormat,
    stop: Option<&AtomicBool>,
) -> Result<u64, Error> {
    read_records(reader, input, |record| {
        let payload = record_payload(record, tags)?;
        if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            return Ok(());
        }
        pool.send_lite3(&payload)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_full_output_shape_is_reserved_and_validated() {
        let message = json!({"seq": 7, "time": "2026-10-14T00:00:00Z",
            "meta": {"tags": ["source"]}, "data": {"x": 1}});
        assert!(record_payload(InputRecord::Json(message.clone()), &[]).is_ok());
        assert!(record_payload(InputRecord::Json(message.clone()), &["outer".into()]).is_err());
        for (field, invalid) in [
            ("seq", Value::Null),
            ("seq", json!(-1)),
            ("seq", json!(1.5)),
            ("time", Value::Null),
            ("time", json!("bad")),
            ("meta", json!({"tags": null})),
            ("meta", json!({"tags": [1]})),
            ("data", json!([])),
            ("data", Value::Null),
        ] {
            let mut value = message.clone();
            value[field] = invalid;
            assert!(
                record_payload(InputRecord::Json(value), &[]).is_err(),
                "{field}"
            );
        }
        for value in [
            json!({"data": {}}),
            json!({"seq": null, "data": {}}),
            json!({"time": "bad", "data": {}}),
            json!({"seq": 7, "time": "bad", "meta": {}, "data": {}}),
            json!({"seq": 7, "time": "bad", "meta": {"tags": [], "app": true}, "data": {}}),
        ] {
            assert!(!is_message_shape(&value));
            assert!(record_payload(InputRecord::Json(value), &[]).is_ok());
        }
    }

    #[test]
    fn stopped_session_does_not_append_buffered_messages() {
        let stop = AtomicBool::new(true);
        let options = IngestOptions {
            tags: &[],
            input: InputFormat::Auto,
            output: OutputMode::Json,
            color: ColorMode::Never,
            emit: false,
            stop: Some(&stop),
        };
        ingest_with_append(&b"{\"data\":{}}\n{\"data\":{}}"[..], options, |_| {
            unreachable!("stopped sender appended");
        })
        .unwrap();
    }
}
