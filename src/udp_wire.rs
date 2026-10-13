//! Shared, private datagram codec compiled into the library and server.
use serde::{Deserialize, Serialize};

pub const MAX_DATAGRAM: usize = 65_507;
pub const HEADER: usize = 24;
pub const DATA_HEADER: usize = 40;
pub const MAX_CONTROL: usize = 4096;
pub const SUBSCRIBE: u8 = 1;
pub const CLOSE: u8 = 2;
pub const CHALLENGE: u8 = 3;
pub const PROGRESS: u8 = 4;
pub const SKIP: u8 = 5;
pub const ERROR: u8 = 6;
pub const DATA: u8 = 7;
pub const LEASE_MS: u64 = 30_000;
pub const RENEW_MS: u64 = 10_000;
pub const STARTUP_MS: u64 = 5_000;
pub type Id = [u8; 16];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Start {
    New,
    Last(u64),
    Since(u64),
    Sequence(u64),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub pool: String,
    pub start: Start,
    pub live: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cookie {
    pub expires_ms: u64,
    pub mac: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subscribe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Selection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<Cookie>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    pub cookie: Cookie,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub start_seq: u64,
    pub next_seq: u64,
    pub end_seq: Option<u64>,
    pub lease_ms: u64,
    pub terminal: bool,
    #[serde(default)]
    pub retention_gap: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Unknown,
    Oversized,
    Retention,
    Pressure,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skip {
    pub first: u64,
    pub last: u64,
    pub reason: Reason,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub kind: String,
    pub message: String,
}

pub fn header(op: u8, id: Id) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER);
    bytes.extend_from_slice(b"\0PUDP\x01");
    bytes.extend_from_slice(&[op, 0]);
    bytes.extend_from_slice(&id);
    bytes
}
pub fn parse(packet: &[u8]) -> Option<(u8, Id, &[u8])> {
    if packet.len() < HEADER
        || packet.len() > MAX_DATAGRAM
        || &packet[..6] != b"\0PUDP\x01"
        || packet[7] != 0
    {
        return None;
    }
    Some((packet[6], packet[8..24].try_into().ok()?, &packet[24..]))
}
pub fn control(op: u8, id: Id, body: &impl Serialize) -> Option<Vec<u8>> {
    let mut bytes = header(op, id);
    bytes.extend_from_slice(&serde_json::to_vec(body).ok()?);
    (bytes.len() <= MAX_CONTROL).then_some(bytes)
}
pub fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Option<T> {
    if body.len() + HEADER > MAX_CONTROL {
        return None;
    }
    serde_json::from_slice(body).ok()
}
pub fn data(id: Id, seq: u64, timestamp_ns: u64, payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() + DATA_HEADER > MAX_DATAGRAM {
        return None;
    }
    let mut bytes = header(DATA, id);
    bytes.extend_from_slice(&seq.to_be_bytes());
    bytes.extend_from_slice(&timestamp_ns.to_be_bytes());
    bytes.extend_from_slice(payload);
    Some(bytes)
}
