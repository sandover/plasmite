//! Purpose: Define the stable public Rust API boundary for Plasmite.
//! Exports: Core types and operations needed by bindings and CLI.
//! Role: Public, additive-only surface; hides internal storage modules.
//! Invariants: This module is the only public path to storage primitives.
//! Invariants: Internal modules remain private and are not directly exposed.

pub mod access;
pub mod browser_trust;
mod client;
mod message;
pub mod notify;
mod remote;
mod udp;
mod validation;

/// Transport and benchmark operations used inside this repository.
#[doc(hidden)]
pub mod internal {
    pub use super::message::LocalPoolTransport;
    pub use super::remote::RemotePoolTransport;
    pub use super::udp::UdpPoolTransport;
}

pub use crate::core::cursor::{Cursor, CursorResult, FrameRef};
#[doc(hidden)]
pub use crate::core::error::to_exit_code;
pub use crate::core::error::{Error, ErrorKind};
pub use crate::core::lite3::{self, Lite3DocRef};
pub use crate::core::pool::{
    AppendOptions, Bounds, Durability, Pool, PoolAgeMetrics, PoolDescription, PoolInfo,
    PoolMetrics, PoolOptions, PoolPathGuard, PoolUtilization, SeqOffsetCache,
};
pub use crate::core::ring_layout::{FrameSpan, RingLayout};
pub use client::{DEFAULT_POOL_SIZE, LocalClient, PoolRef};
pub use message::{Feed, FeedInput, Follow, Message, Read, Since};
#[doc(hidden)]
pub use message::{
    FeedReceipt, HostedRead, Lite3Follow, MessageEnvelope, Meta, ReadPoll, ReadStatus, ViewFollow,
    message_json_from_frame,
};
pub(crate) use message::{
    TailOutcome, TailState, format_ts, frame_matches_tags, message_from_udp_payload,
    select_frame_json, select_lite3_frame, select_view,
};
pub(crate) use remote::{AsyncRead as AsyncHttpRead, stream_runtime};
pub use remote::{RemoteClient, RemoteLite3Frame, RemoteLite3Read, RemotePool, RemoteRead};
pub(crate) use udp::AsyncUdpRead;
pub use udp::{UdpClient, UdpEvent, UdpFollow, UdpPool, UdpSkipReason, UdpStart, UdpStreamEvent};
#[doc(hidden)]
pub use udp::{UdpFrame, UdpRawEvent, UdpRawFollow, UdpViewEvent, UdpViewFollow};
pub use validation::{
    ValidationIssue, ValidationMessageStats, ValidationObservation, ValidationReport,
    ValidationStatus,
};

#[cfg(test)]
pub(crate) use message::PoolApiExt;
