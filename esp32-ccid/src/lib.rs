//! ESP32 + PN532 CCID-over-serial firmware library
//!
//! This library provides CCID protocol implementation for ESP32 with PN532
//! NFC controller over SPI, emulating a GemPC Twin serial reader.
//!
//! # Architecture
//!
//! - **ccid_types**: CCID message structures and constants
//! - **nfc**: PN532 NFC controller interface (SPI)
//! - **serial_framing**: CCID-over-serial framing protocol
//! - **ccid_handler**: CCID command handling logic

pub mod ccid_handler;
pub mod ccid_types;
pub mod nfc;
pub mod pn532_driver;
pub mod serial_framing;
