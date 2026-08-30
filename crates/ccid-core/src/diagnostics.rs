//! Runtime diagnostics counters.
//!
//! The `Diagnostics` struct and its frozen 28-byte little-endian wire format
//! (CCID Escape 0xD0) live in the canonical `amp-embedded-common` repo
//! (`amp-diagnostics` crate, rev-pinned git dependency); this module
//! re-exports it so the `ccid_core::diagnostics::Diagnostics` and
//! `ccid_core::Diagnostics` paths stay stable.

pub use amp_diagnostics::Diagnostics;
