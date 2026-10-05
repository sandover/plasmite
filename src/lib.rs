//! Purpose: Shared library crate used by the `plasmite` CLI and bindings.
//! Exports: `api` (stable public surface), `notice` (structured stderr notices).
//! Role: Public API boundary with private internal storage modules.
//! Invariants: Additive-only changes to `api`; internal modules remain private.
//! Invariants: Core modules prefer explicit inputs/outputs over hidden state.
mod abi;
pub mod api;
mod core;
mod interface_wire;
pub mod mcp;
pub mod notice;
mod pool_paths;
mod since;
#[cfg(windows)]
#[expect(
    dead_code,
    reason = "The CLI also compiles this module and uses its server-only Windows service policies."
)]
mod windows_private;
