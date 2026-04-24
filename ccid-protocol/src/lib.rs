//! Shared CCID protocol types, constants, and utilities.
//!
//! This crate contains CCID Rev 1.1 protocol definitions shared between
//! the STM32 USB CCID firmware and the ESP32 serial CCID firmware.
//!
//! # Modules
//!
//! - [`types`] — CCID message header, slot state, constants
//! - [`status`] — Status byte encoding helpers
//! - [`atr`] — ATR (Answer to Reset) parsing per ISO 7816-3

#![no_std]

pub mod atr;
pub mod status;
pub mod types;
