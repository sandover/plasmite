//! Purpose: Define the stable public Rust API boundary for Plasmite.
//! Exports: Core types and operations needed by bindings and CLI.
//! Role: Public, additive-only surface; hides internal storage modules.
//! Invariants: This module is the only public path to storage primitives.
//! Invariants: Internal modules remain private and are not directly exposed.

mod client;
mod message;
mod remote;
mod validation;

pub use crate::core::cursor::{Cursor, CursorResult, FrameRef};
#[doc(hidden)]
pub use crate::core::error::to_exit_code;
pub use crate::core::error::{Error, ErrorKind};
pub use crate::core::lite3::{self, Lite3DocRef};
pub use crate::core::pool::{AppendOptions, Bounds, Durability, Pool, PoolInfo, PoolOptions};
pub use client::{LocalClient, PoolRef};
pub use message::{Message, Meta, PoolApiExt, Tail, TailOptions};
pub use remote::{RemoteClient, RemotePool, RemoteTail};
pub use validation::{ValidationIssue, ValidationReport, ValidationStatus};
