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

pub use crate::core::cursor::{Cursor, CursorResult, FrameRef};
#[doc(hidden)]
pub use crate::core::error::to_exit_code;
pub use crate::core::error::{Error, ErrorKind};
pub use crate::core::lite3::{self, Lite3DocRef};
pub use crate::core::pool::{
    AppendOptions, Bounds, Durability, Pool, PoolAgeMetrics, PoolInfo, PoolMetrics, PoolOptions,
    PoolPathGuard, PoolUtilization, SeqOffsetCache,
};
pub use crate::core::ring_layout::{FrameSpan, RingLayout};
pub use client::{DEFAULT_POOL_SIZE, LocalClient, PoolRef};
pub use message::{
    Feed, FeedInput, FeedReceipt, Follow, Lite3Follow, Message, Meta, Read, ReadPoll, Since,
    ViewFollow,
};
#[doc(hidden)]
pub use message::{ReadStatus, message_json_from_frame};
pub(crate) use message::{
    TailOutcome, TailState, format_ts, frame_matches_tags, message_from_udp_payload,
    select_frame_json, select_lite3_frame, select_view,
};
pub use remote::{RemoteClient, RemoteLite3Frame, RemoteLite3Read, RemotePool, RemoteRead};
pub use udp::{
    UdpClient, UdpEvent, UdpFollow, UdpFrame, UdpPool, UdpSkipReason, UdpStart, UdpStreamEvent,
    UdpViewEvent, UdpViewFollow,
};
pub use validation::{
    ValidationIssue, ValidationMessageStats, ValidationObservation, ValidationReport,
    ValidationStatus,
};

#[cfg(test)]
pub(crate) use message::PoolApiExt;
