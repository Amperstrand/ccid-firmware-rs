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
pub mod ccid_serial_server;
pub mod ccid_types;
pub mod nfc;
pub mod pn532_driver;
pub mod serial_framing;

#[cfg(feature = "backend-mfrc522")]
pub mod mfrc522_driver;

#[cfg(feature = "backend-pn7160")]
pub mod pn7160_driver;

#[cfg(all(
    feature = "backend-pn7160",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod pn7160_i2c;

#[cfg(all(
    feature = "pn7160-bringup",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod pn7160_bringup;

#[cfg(all(
    feature = "pn7160-ccid",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod pn7160_ccid;

#[cfg(all(
    feature = "bench-net",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod netlog;

#[cfg(all(
    feature = "bench-net",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod ota;

#[cfg(all(
    feature = "bench-net",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod wifi;

#[cfg(feature = "backend-mfrc522")]
pub mod led;

#[cfg(all(feature = "backend-mfrc522", feature = "ble"))]
pub mod ble_debug;

#[cfg(all(feature = "backend-mfrc522", feature = "ble"))]
pub mod ble_logger;
