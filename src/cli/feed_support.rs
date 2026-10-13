//! Append complete CLI messages through the same local and remote API paths.

use super::message_stream::{InputFormat, InputRecord, read_records};
use super::output::OutputMode;
use super::output_support::{emit_lite3, emit_message};
use super::support::{RetryConfig, message_to_json, retry_append};
use crate::ColorMode;
use plasmite::api::{
    AppendOptions, Durability, Error, ErrorKind, Message, Pool, PoolApiExt, RemotePool, lite3,
};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageInput {
    #[serde(default, deserialize_with = "present_value")]
    seq: Option<u64>,
    #[serde(default, deserialize_with = "present_value")]
    time: Option<String>,
    #[serde(default)]
    meta: MessageMeta,
    data: Value,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageMeta {
    #[serde(default)]
    tags: Vec<String>,
}

fn present_value<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    reader: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(reader).map(Some)
}

pub(crate) fn record_payload(
    record: InputRecord,
    contents: bool,
    tags: &[String],
) -> Result<Vec<u8>, Error> {
    match record {
        InputRecord::Lite3 {
            seq,
            timestamp_ns,
            payload,
        } => {
            let _ = (seq, timestamp_ns);
            if contents || !tags.is_empty() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("Lite3 already contains a complete message")
                    .with_hint("Remove --data and --tag for Lite3 input."));
            }
            Ok(payload)
        }
        InputRecord::Json(value) => {
            let (data, tags) = if contents {
                (value, tags.to_vec())
            } else {
                let message: MessageInput = serde_json::from_value(value).map_err(|err| {
                    Error::new(ErrorKind::Usage)
                        .with_message("expected a complete message with an object data field")
                        .with_hint("Use --data to create messages from application JSON.")
                        .with_source(err)
                })?;
                // Identity belongs to the source pool. The append assigns its own identity.
                let _ = message.seq;
                if let Some(time) = message.time {
                    time::OffsetDateTime::parse(
                        &time,
                        &time::format_description::well_known::Rfc3339,
                    )
                    .map_err(|err| {
                        Error::new(ErrorKind::Usage)
                            .with_message("invalid source message time")
                            .with_hint("Use an RFC 3339 timestamp or omit time.")
                            .with_source(err)
                    })?;
                }
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
    Error::new(ErrorKind::Usage).with_message("missing message input").with_hint("Pipe messages to feed, use --file, or provide a message argument. Use --data to create a message.")
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
    pub(crate) contents: bool,
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
    pub(crate) contents: bool,
    pub(crate) stop: Option<&'a AtomicBool>,
}

#[derive(Copy, Clone)]
struct IngestOptions<'a> {
    contents: bool,
    tags: &'a [String],
    input: InputFormat,
    output: OutputMode,
    color: ColorMode,
    emit: bool,
    stop: Option<&'a AtomicBool>,
}

fn ingest_with_append(
    reader: impl Read,
    options: IngestOptions<'_>,
    mut append: impl FnMut(&[u8]) -> Result<Message, Error>,
) -> Result<u64, Error> {
    let count = read_records(reader, options.input, |record| {
        let payload = record_payload(record, options.contents, options.tags)?;
        if options
            .stop
            .is_some_and(|stop| stop.load(Ordering::Acquire))
        {
            return Ok(());
        }
        let message = append(&payload)?;
        // Storage succeeded. A later stdout error stops ingestion; it must never retry this append.
        if options.emit {
            if options.output.is_binary() {
                let ns = message_timestamp(&message)?;
                emit_lite3(message.seq, ns, &payload, options.output)?;
            } else {
                emit_message(message_to_json(&message), options.output, options.color)?;
            }
        }
        Ok(())
    })?;
    Ok(count)
}

pub(crate) fn message_timestamp(message: &Message) -> Result<u64, Error> {
    let time = time::OffsetDateTime::parse(
        &message.time,
        &time::format_description::well_known::Rfc3339,
    )
    .map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("invalid message time")
            .with_source(err)
    })?;
    u64::try_from(time.unix_timestamp_nanos()).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("message time is out of range")
            .with_source(err)
    })
}

pub(crate) fn ingest_from_stdin(
    reader: impl Read,
    ctx: FeedIngestContext<'_>,
    emit_receipt: bool,
) -> Result<u64, Error> {
    let options = IngestOptions {
        contents: ctx.contents,
        tags: ctx.tags,
        input: ctx.input,
        output: ctx.output_mode,
        color: ctx.color_mode,
        emit: emit_receipt,
        stop: ctx.stop,
    };
    ingest_with_append(reader, options, |payload| {
        retry_append(ctx.retry_config, || {
            ctx.pool_handle
                .append_lite3_with_receipt_now(payload, ctx.durability)
        })
    })
}

pub(crate) fn ingest_from_stdin_remote(
    reader: impl Read,
    ctx: RemoteFeedIngestContext<'_>,
    emit_receipt: bool,
) -> Result<u64, Error> {
    let options = IngestOptions {
        contents: ctx.contents,
        tags: ctx.tags,
        input: ctx.input,
        output: ctx.output_mode,
        color: ctx.color_mode,
        emit: emit_receipt,
        stop: ctx.stop,
    };
    ingest_with_append(reader, options, |payload| {
        retry_append(ctx.retry_config, || {
            ctx.remote_pool
                .append_lite3_with_receipt(payload, AppendOptions::new(0, ctx.durability))
        })
    })
}

pub(crate) fn ingest_from_stdin_udp(
    reader: impl Read,
    pool: &plasmite::api::UdpPool,
    tags: &[String],
    input: InputFormat,
    contents: bool,
    stop: Option<&AtomicBool>,
) -> Result<u64, Error> {
    read_records(reader, input, |record| {
        let payload = record_payload(record, contents, tags)?;
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
    fn source_identity_rejects_null_but_allows_omission() {
        for value in [
            json!({"seq":null,"data":{}}),
            json!({"time":null,"data":{}}),
            json!({"time":"bad","data":{}}),
        ] {
            assert!(record_payload(InputRecord::Json(value), false, &[]).is_err());
        }
        assert!(record_payload(InputRecord::Json(json!({"data":{}})), false, &[]).is_ok());
    }

    #[test]
    fn stopped_session_does_not_append_buffered_messages() {
        let stop = AtomicBool::new(true);
        let options = IngestOptions {
            contents: false,
            tags: &[],
            input: InputFormat::Auto,
            output: OutputMode::Json,
            color: ColorMode::Never,
            emit: false,
            stop: Some(&stop),
        };
        let mut writes = 0;
        ingest_with_append(&b"{\"data\":{}}\n{\"data\":{}}"[..], options, |_| {
            writes += 1;
            unreachable!("stopped sender appended");
        })
        .unwrap();
        assert_eq!(writes, 0);
    }
}
