//! Pinpad module for secure PIN entry
//!
//! This module provides secure PIN entry functionality for CCID smartcard readers.
//! It includes:
//! - APDU construction for VERIFY commands
//! - State machine for PIN entry flow
//! - Touchscreen-based pinpad UI (embedded-graphics)
//!
//! The CCID §6.1.11/12 wire structures (PinVerifyParams, PinModifyParams,
//! PinResult, PinBuffer) AND the reader-built VERIFY/CHANGE APDU builders
//! live in the shared `ccid-core` secure module (DRY phase 2 / #121
//! track I) and are re-exported here for path compatibility.
#![allow(dead_code)]
#![allow(unused_imports)]
//!
//! The module is designed to work on both embedded ARM targets and host machines
//! for testing. All modules use embedded-graphics which works on both platforms.

pub mod apdu;
pub mod state;
#[cfg(feature = "display")]
pub mod ui;

// Re-export common types
pub use apdu::{ApduError, ModifyApduBuilder, VerifyApduBuilder, VerifyResponse};
pub use state::{PinEntryContext, PinEntryState, PinModifyContext, PinModifyStep};
#[cfg(feature = "display")]
pub use ui::{draw_pinpad, ButtonId, Keypad, TouchHandler};

// CCID §6.1.11/12 wire structures: shared with the handler core.
pub use ::ccid_core::secure::{PinBuffer, PinModifyParams, PinResult, PinVerifyParams};
