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

// CCID_SPEC: /* Identifies the length of type of subordinate descriptors of a CCID device
// * Table 5.1-1 Smart Card Device Class descriptors */
// struct usb_ccid_class_descriptor {

pub mod atr;
pub mod status;
pub mod types;
