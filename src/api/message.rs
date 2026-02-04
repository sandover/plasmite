//! Purpose: Define public message types and append/get/tail helpers for the API.
//! Exports: `Message`, `Meta`, `TailOptions`, `Tail`.
//! Role: Stable message envelope aligned with the CLI contract.
//! Invariants: Message fields mirror CLI JSON; time is RFC3339 UTC.
//! Invariants: Tail streams preserve ordering and avoid unbounded buffering.
#![allow(clippy::result_large_err)]

use crate::core::cursor::{Cursor, CursorResult, FrameRef};
use crate::core::error::{Error, ErrorKind};
use crate::core::lite3::Lite3DocRef;
use crate::core::pool::{AppendOptions, Durability, Pool};
use serde_json::Value;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Meta {
    pub descrips: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub seq: u64,
    pub time: String,
    pub meta: Meta,
    pub data: Value,
}

#[derive(Clone, Debug)]
pub struct TailOptions {
    pub since_seq: Option<u64>,
    pub max_messages: Option<usize>,
    pub poll_interval: Duration,
    pub timeout: Option<Duration>,
}

impl TailOptions {
    pub fn new() -> Self {
        Self {
            since_seq: None,
            max_messages: None,
            poll_interval: Duration::from_millis(50),
            timeout: None,
        }
    }
}

impl Default for TailOptions {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Tail<'a> {
    pool: &'a Pool,
    cursor: Cursor,
    options: TailOptions,
    seen: usize,
    deadline: Option<Instant>,
}

impl<'a> Tail<'a> {
    fn new(pool: &'a Pool, options: TailOptions) -> Self {
        let deadline = options.timeout.map(|duration| Instant::now() + duration);
        Self {
            pool,
            cursor: Cursor::new(),
            options,
            seen: 0,
            deadline,
        }
    }

    pub fn next_message(&mut self) -> Result<Option<Message>, Error> {
        if let Some(max) = self.options.max_messages {
            if self.seen >= max {
                return Ok(None);
            }
        }

        loop {
            if let Some(deadline) = self.deadline {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
            }

            match self.cursor.next(self.pool)? {
                CursorResult::Message(frame) => {
                    if let Some(min_seq) = self.options.since_seq {
                        if frame.seq < min_seq {
                            continue;
                        }
                    }
                    let message = message_from_frame(&frame)?;
                    self.seen += 1;
                    return Ok(Some(message));
                }
                CursorResult::WouldBlock => {
                    std::thread::sleep(self.options.poll_interval);
                }
                CursorResult::FellBehind => {
                    continue;
                }
            }
        }
    }
}

pub trait PoolApiExt {
    fn append_json(
        &mut self,
        data: &Value,
        descrips: &[String],
        options: AppendOptions,
    ) -> Result<Message, Error>;

    fn append_json_now(
        &mut self,
        data: &Value,
        descrips: &[String],
        durability: Durability,
    ) -> Result<Message, Error>;

    fn get_message(&self, seq: u64) -> Result<Message, Error>;

    fn tail(&self, options: TailOptions) -> Tail<'_>;
}

impl PoolApiExt for Pool {
    fn append_json(
        &mut self,
        data: &Value,
        descrips: &[String],
        options: AppendOptions,
    ) -> Result<Message, Error> {
        let payload = crate::core::lite3::encode_message(descrips, data)?;
        let seq = self.append_with_options(payload.as_slice(), options)?;
        Ok(Message {
            seq,
            time: format_ts(options.timestamp_ns)?,
            meta: Meta {
                descrips: descrips.to_vec(),
            },
            data: data.clone(),
        })
    }

    fn append_json_now(
        &mut self,
        data: &Value,
        descrips: &[String],
        durability: Durability,
    ) -> Result<Message, Error> {
        let timestamp_ns = now_ns()?;
        let options = AppendOptions::new(timestamp_ns, durability);
        self.append_json(data, descrips, options)
    }

    fn get_message(&self, seq: u64) -> Result<Message, Error> {
        let frame = self.get(seq)?;
        message_from_frame(&frame)
    }

    fn tail(&self, options: TailOptions) -> Tail<'_> {
        Tail::new(self, options)
    }
}

fn message_from_frame(frame: &FrameRef<'_>) -> Result<Message, Error> {
    let (meta, data) = decode_payload(frame.payload)?;
    Ok(Message {
        seq: frame.seq,
        time: format_ts(frame.timestamp_ns)?,
        meta,
        data,
    })
}

fn decode_payload(payload: &[u8]) -> Result<(Meta, Value), Error> {
    let json_str = Lite3DocRef::new(payload).to_json(false)?;
    let value: Value = serde_json::from_str(&json_str).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid payload json")
            .with_source(err)
    })?;
    let obj = value
        .as_object()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("payload is not object"))?;
    let meta_value = obj
        .get("meta")
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("missing meta"))?;
    let data = obj
        .get("data")
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("missing data"))?;

    let meta_obj = meta_value
        .as_object()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("meta is not object"))?;
    let descrips_value = meta_obj
        .get("descrips")
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("missing meta.descrips"))?;
    let descrips = descrips_value
        .as_array()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("meta.descrips must be array"))?
        .iter()
        .map(|item| item.as_str().map(|s| s.to_string()))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            Error::new(ErrorKind::Corrupt).with_message("meta.descrips must be string array")
        })?;

    Ok((Meta { descrips }, data))
}

fn now_ns() -> Result<u64, Error> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("time went backwards")
                .with_source(err)
        })?;
    Ok(duration.as_nanos() as u64)
}

fn format_ts(timestamp_ns: u64) -> Result<String, Error> {
    use time::format_description::well_known::Rfc3339;
    let ts =
        time::OffsetDateTime::from_unix_timestamp_nanos(timestamp_ns as i128).map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("invalid timestamp")
                .with_source(err)
        })?;
    ts.format(&Rfc3339).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("timestamp format failed")
            .with_source(err)
    })
}

#[cfg(test)]
mod tests {
    use super::{Meta, decode_payload};
    use crate::core::lite3::encode_message;
    use serde_json::json;

    #[test]
    fn decode_payload_round_trip() {
        let data = json!({"x": 1});
        let payload = encode_message(&["tag".to_string()], &data).expect("encode");
        let (meta, out) = decode_payload(payload.as_slice()).expect("decode");
        assert_eq!(
            meta,
            Meta {
                descrips: vec!["tag".to_string()]
            }
        );
        assert_eq!(out, data);
    }
}
