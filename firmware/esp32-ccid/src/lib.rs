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

#[cfg(test)]
pub mod ccid_fuzz;
pub mod ccid_handler;
pub mod ccid_serial_server;
pub mod ccid_types;
pub mod frontend;
pub mod m5stick_lcd;
pub mod nfc;
pub mod ota_header;
pub mod pn532_driver;
pub mod serial_framing;
pub mod t1;
#[cfg(test)]
pub mod t1_fuzz;
pub mod turnaround;

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
    feature = "pn7160-bitbang",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod pn7160_bitbang;

#[cfg(all(
    feature = "pn7160-m1",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod pn7160_m1;

#[cfg(all(
    feature = "pn7160-bringup",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod pn7160_bringup;
#[cfg(all(
    feature = "pn7160-v10raw",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod pn7160_v10raw;

#[cfg(all(
    feature = "pn7160-actdiag",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod pn7160_actdiag;

#[cfg(all(
    feature = "pn7160-ccid",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod pn7160_ccid;

#[cfg(all(
    feature = "wifi-ccid",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod wifi_ccid;

#[cfg(all(
    any(feature = "wifi-ccid", feature = "wifi-vpcd"),
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod wifi_common;

#[cfg(all(
    feature = "wifi-vpcd",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod wifi_vpcd;

#[cfg(all(
    any(
        feature = "pn7160-bringup",
        feature = "backend-mfrc522",
        feature = "wifi-ccid",
        feature = "wifi-vpcd"
    ),
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod netlog;

#[cfg(all(
    feature = "bench-net",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod ota;

#[cfg(all(
    feature = "wifi-vpcd",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod vpcd;

#[cfg(all(
    any(
        feature = "pn7160-bringup",
        feature = "wifi-ccid",
        feature = "wifi-vpcd",
        feature = "bench-net"
    ),
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
pub mod wifi;

#[cfg(feature = "backend-mfrc522")]
pub mod led;

/// Host-testable log ring shared by the BLE debug logger shells.
pub mod ble_log_queue;

/// pcscd-over-Bluetooth-SPP transport: pure serving glue (host-tested,
/// always compiled — `ble_log_queue` pattern) + a target shell gated on
/// the `spp` feature inside the module.
pub mod spp_server;

#[cfg(feature = "ble")]
pub mod ble_debug;

#[cfg(feature = "ble")]
pub mod ble_logger;

#[cfg(all(feature = "ble", any(target_arch = "xtensa", target_arch = "riscv32")))]
pub mod ble_console;
