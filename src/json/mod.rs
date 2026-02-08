//! Purpose: Internal JSON parsing boundary shared by runtime callsites.
//! Exports: `parse` module with decode helpers used by CLI and API internals.
//! Role: Single seam for parser implementation so callsites avoid ad hoc decode logic.
//! Invariants: Runtime JSON decoding goes through this module in migrated paths.
//! Invariants: Helper APIs stay small and deterministic (no hidden global state).

pub(crate) mod parse;
