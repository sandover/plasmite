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
pub use client::{LocalClient, PoolRef};
pub use message::{GapPolicy, Lite3Tail, Message, Meta, PoolApiExt, Tail, TailOptions, ViewTail};
pub(crate) use message::{
    TailOutcome, TailState, format_ts, message_from_udp_payload, message_json_from_frame, now_ns,
};
pub use remote::{RemoteClient, RemoteLite3Frame, RemoteLite3Tail, RemotePool, RemoteTail};
pub use udp::{
    UdpClient, UdpEvent, UdpFollow, UdpFollowOptions, UdpFrame, UdpPool, UdpSkipReason, UdpStart,
    UdpStreamEvent, UdpViewEvent, UdpViewFollow,
};
pub use validation::{
    ValidationIssue, ValidationMessageStats, ValidationObservation, ValidationReport,
    ValidationStatus,
};
