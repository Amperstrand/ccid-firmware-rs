//! Frontend diagnostics trait surface (kanban t_4864e74e).
//!
//! One uniform, board-blind way to ask any NFC frontend "how are you?"
//! — the 0xD0 presence/reinit counters and the bring-up self-test,
//! promoted from per-driver ad-hoc surface to a trait so HIL drives
//! frontends uniformly and the serving kernel can log health without
//! seeing board internals.
//!
//! # Home justification
//!
//! Homed in `card-interface` — the kernel's lowest, zero-dependency
//! trait crate — because every party to the contract already depends on
//! it and nothing else may:
//!
//! - `ccid-service` may depend ONLY on card-interface / ccid-core /
//!   ccid-protocol / ccid-transport-serial (its Cargo.toml boundary
//!   rule; `tools/union_matrix.py check` enforces it), so a trait homed
//!   there is reachable from the serving kernel's logging path without
//!   a new dependency edge — the boundary is strengthened, not bent.
//! - The frontend crates (`mfrc522-pcd`, `pn7160-nci`) and firmware-bin
//!   drivers (`mfrc522_driver`, `pn532_driver`, `pn7160_driver`) are
//!   `no_std`/embedded — they can implement a trait from this
//!   `#![no_std]`, dependency-free crate, but never one from a std or
//!   esp-bound crate.
//! - `boards` stays pure data (BOARDS-AS-DATA-DESIGN §3.2: board facts
//!   reach the service only compiled INTO a constructed driver); this
//!   trait is the board-free shape those drivers answer with.
//!
//! Everything in the surface is core-only: enums, counters, plain
//! structs. No pins, no peripherals, no board tables, no bus handles.

use core::result::Result;

/// Coarse frontend health for logs and bench probes.
///
/// Maps 1:1 onto the serving kernel's wrapper states
/// (`ccid_service::frontend::{RetryFrontend, UnavailableNfcDriver}`):
/// Healthy serves normally, Degraded serves card-absent while init is
/// retried, Unavailable means the controller could not even be
/// constructed (stand-in driver).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontendHealth {
    /// Constructed and initialized; serving normally.
    Healthy,
    /// Constructed but init() currently failing: card-absent, retrying.
    Degraded,
    /// Never constructed (hw init failure) — stand-in driver serving.
    Unavailable,
}

/// Board-free classification of the frontend's last error, for logs.
///
/// Deliberately coarse: driver error enums are backend-specific (and
/// some carry hardware detail); this is the common vocabulary the
/// serving kernel and HIL probes read. #257 error-model pass:
/// `Display`/`core::error::Error` derived (additive — this enum is the
/// natural target vocabulary when the NfcError twins collapse, still
/// P5-sequenced).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrontendErrorKind {
    /// No error recorded since the last success.
    #[error("no error")]
    None,
    /// Driver not initialized (or init failed).
    #[error("driver not initialized")]
    NotInitialized,
    /// Bus-level failure (I2C NAK, SPI fault, bus wedged).
    #[error("bus-level failure")]
    Bus,
    /// Operation timed out (controller unresponsive).
    #[error("operation timed out")]
    Timeout,
    /// Protocol-level failure (framing, CRC, bad version).
    #[error("protocol-level failure")]
    Protocol,
    /// No card present for a card-requiring operation.
    #[error("no card present")]
    NoCard,
    /// Buffer too small for the exchanged data.
    #[error("buffer too small")]
    BufferOverflow,
}

/// A frontend's diagnostics snapshot: health, last error and the
/// counters behind CCID Escape 0xD0's presence/reinit words plus the
/// bench probes. `Copy` by design — pulled, not held (the logging
/// doctrine's pull-not-push rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrontendStatus {
    /// Current health classification.
    pub health: FrontendHealth,
    /// Last observed error, board-free classification.
    pub last_error: FrontendErrorKind,
    /// Init attempts since boot (constructor ladder + retry polls).
    pub init_attempts: u32,
    /// Driver-level self-healing reinitializations since boot (the
    /// 0xD0 `reinit_count` word's frontend source).
    pub reinit_count: u32,
    /// Bus/communication failures since boot.
    pub bus_errors: u32,
    /// Presence polls served since boot (the 0xD0 presence source).
    pub presence_polls: u32,
}

impl FrontendStatus {
    /// All-zero Healthy snapshot — the "no news" baseline.
    pub const fn ok() -> Self {
        Self {
            health: FrontendHealth::Healthy,
            last_error: FrontendErrorKind::None,
            init_attempts: 0,
            reinit_count: 0,
            bus_errors: 0,
            presence_polls: 0,
        }
    }
}

impl Default for FrontendStatus {
    fn default() -> Self {
        Self::ok()
    }
}

/// Uniform diagnostics surface for NFC frontends (t_4864e74e).
///
/// Implemented by every `NfcDriver` backend and by the serving
/// kernel's wrappers, so health/logs/bench probes drive all frontends
/// — MFRC522, PN532, PN7160 — through one shape. Pure data out; no
/// board internals cross this boundary.
pub trait FrontendDiagnostics {
    /// Pull the current diagnostics snapshot.
    fn frontend_status(&self) -> FrontendStatus;

    /// Bring-up self-test: probe the controller on its bus and confirm
    /// it answers as the expected part (e.g. MFRC522 VersionReg
    /// 0x91/0x92, PN532 GetFirmwareVersion, PN7160 core reset ACK).
    /// Must be safe on a healthy serving frontend (stateless probe
    /// class only — never a state-machine-touching transact).
    fn frontend_self_test(&mut self) -> Result<(), FrontendErrorKind>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #257 error-model pass lock: the additive Display strings are
    /// part of the log vocabulary now — byte-locked. (`to_string` via
    /// an explicit import: the crate is no_std, the std prelude is not
    /// injected in test builds.)
    #[test]
    fn frontend_error_kind_display_strings_are_locked() {
        use std::string::ToString as _;
        assert_eq!(FrontendErrorKind::None.to_string(), "no error");
        assert_eq!(
            FrontendErrorKind::NotInitialized.to_string(),
            "driver not initialized"
        );
        assert_eq!(FrontendErrorKind::Bus.to_string(), "bus-level failure");
        assert_eq!(
            FrontendErrorKind::Timeout.to_string(),
            "operation timed out"
        );
        assert_eq!(
            FrontendErrorKind::Protocol.to_string(),
            "protocol-level failure"
        );
        assert_eq!(FrontendErrorKind::NoCard.to_string(), "no card present");
        assert_eq!(
            FrontendErrorKind::BufferOverflow.to_string(),
            "buffer too small"
        );
        let _: &dyn core::error::Error = &FrontendErrorKind::Bus;
    }

    #[test]
    fn ok_snapshot_is_healthy_with_zero_counters() {
        let s = FrontendStatus::ok();
        assert_eq!(s.health, FrontendHealth::Healthy);
        assert_eq!(s.last_error, FrontendErrorKind::None);
        assert_eq!(s.init_attempts, 0);
        assert_eq!(s.reinit_count, 0);
        assert_eq!(s.bus_errors, 0);
        assert_eq!(s.presence_polls, 0);
        assert_eq!(FrontendStatus::default(), s);
    }

    #[test]
    fn health_and_error_kinds_are_distinct_and_comparable() {
        let states = [
            FrontendHealth::Healthy,
            FrontendHealth::Degraded,
            FrontendHealth::Unavailable,
        ];
        for (i, a) in states.iter().enumerate() {
            for (j, b) in states.iter().enumerate() {
                assert_eq!(a == b, i == j);
            }
        }
        let kinds = [
            FrontendErrorKind::None,
            FrontendErrorKind::NotInitialized,
            FrontendErrorKind::Bus,
            FrontendErrorKind::Timeout,
            FrontendErrorKind::Protocol,
            FrontendErrorKind::NoCard,
            FrontendErrorKind::BufferOverflow,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for (j, b) in kinds.iter().enumerate() {
                assert_eq!(a == b, i == j);
            }
        }
    }
}
