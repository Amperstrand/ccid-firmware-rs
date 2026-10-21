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
// Serving-kernel modules extracted to crates/ccid-service (t_72e1e2ed):
// re-exported under their historical paths so mains, drivers and tests
// keep compiling unchanged. The crate boundary (ccid-service sees only
// card-interface + ccid-core/protocol/transport-serial) is what makes
// "works on any board" compiler-enforced.
pub use ccid_service::ccid_handler;
pub use ccid_service::ccid_serial_server;
pub use ccid_service::ccid_types;
pub use ccid_service::frontend;
pub use ccid_service::nfc;
pub use ccid_service::t1;
pub use ccid_service::turnaround;

/// Hand the kernel the platform monotonic clock (t_72e1e2ed): the
/// serving kernel injects time (`ccid_service::t1::install_clock`) and
/// carries no ESP-IDF dependency. Every target main that serves CCID
/// calls this before constructing the handler — before it, the kernel
/// clock reads 0 (the historical host-build behavior).
#[cfg(any(target_arch = "xtensa", target_arch = "riscv32"))]
pub fn install_platform_clock() {
    ccid_service::t1::install_clock(|| unsafe { esp_idf_sys::esp_timer_get_time() });
}

pub mod m5stick_lcd;
pub mod ota_header;
pub mod pn532_driver;
pub mod serial_framing;
#[cfg(test)]
pub mod t1_fuzz;

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

// x86_64: pure std, always host-tested in ccid-service. Targets:
// re-exported for wifi-vpcd builds only (kernel compiles it always).
#[cfg(any(
    target_arch = "x86_64",
    all(
        feature = "wifi-vpcd",
        any(target_arch = "xtensa", target_arch = "riscv32")
    )
))]
pub use ccid_service::vpcd;

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

/// pcscd-over-Bluetooth-SPP transport (t_72e1e2ed): the pure,
/// host-tested serving pump lives in the kernel crate; this firmware
/// shell (bring-up + RFCOMM event loop) is gated on the `spp` feature.
pub use ccid_service::spp_server;
#[cfg(all(feature = "spp", any(target_arch = "xtensa", target_arch = "riscv32")))]
pub mod spp_shell;

pub mod ble_addr;

#[cfg(feature = "ble")]
pub mod ble_debug;

#[cfg(feature = "ble")]
pub mod ble_logger;

#[cfg(all(feature = "ble", any(target_arch = "xtensa", target_arch = "riscv32")))]
pub mod ble_console;
