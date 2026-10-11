//! Re-export surface for the protocol helpers shared from
//! `ccid-protocol` (kept so firmware-side paths stay stable after the
//! crate extraction). The unit tests that used to live here duplicated
//! the `ccid-protocol` crate's own suite verbatim — same assertions run
//! in the same `cargo test --workspace` invocation — and modeled the
//! same citation gaps (see docs/AUDIT_FINDINGS.md #13), so they were
//! dropped in favor of the crate tests they mirrored.

pub use ccid_protocol::atr::*;
pub use ccid_protocol::status::build_bstatus;
