//! ESP32-S3 NFC CCID Reader — Entry Point
//!
//! This is the firmware entry point for the NFC variant of ccid-firmware-rs,
//! running on ESP32-S3 with a PN532 NFC frontend over SPI.
//!
//! # Architecture
//!
//! ```text
//! Host PC ←→ [USB CCID] ←→ ESP32-S3 ←→ [SPI] ←→ PN532 ←→ [RF] ←→ NFC Card
//! ```
//!
//! The ESP32-S3 was chosen because it has **native USB OTG** support,
//! which is required to implement a USB CCID device class. The original
//! ESP32 and ESP32-C3/C6 do NOT have USB device (OTG) capability —
//! they only have USB-UART bridges or USB Serial/JTAG, which cannot
//! present custom USB classes like CCID.
//!
//! # Hardware Requirements
//!
//! - **ESP32-S3** dev board (e.g., ESP32-S3-DevKitC-1)
//! - **PN532** NFC module (SPI mode)
//! - USB cable for host connection (via native USB, NOT the UART port)
//!
//! # Wiring (Bolty-style)
//!
//! | PN532 Pin | ESP32-S3 Pin | Function        |
//! |-----------|-------------|-----------------|
//! | SCK       | GPIO 12     | SPI Clock       |
//! | MOSI      | GPIO 11     | SPI Data Out    |
//! | MISO      | GPIO 13     | SPI Data In     |
//! | SS/CS     | GPIO 10     | SPI Chip Select |
//! | VCC       | 3.3V        | Power           |
//! | GND       | GND         | Ground          |
//!
//! > Set PN532 DIP switches to SPI mode (typically: SEL0=LOW, SEL1=HIGH)
//!
//! # Build Instructions
//!
//! See BUILDING.md for full instructions. Quick start:
//!
//! ```bash
//! # Install ESP32 Rust toolchain
//! cargo install espup
//! espup install
//! . ~/export-esp.sh
//!
//! # Build
//! cargo build --release --features nfc --no-default-features --bin ccid-nfc \
//!     --target xtensa-esp32s3-none-elf
//!
//! # Flash
//! espflash flash target/xtensa-esp32s3-none-elf/release/ccid-nfc
//! ```
//!
//! # Current Status
//!
//! This is a **template / scaffold** for the ESP32-S3 NFC CCID reader.
//! The core CCID protocol and PN532 driver are implemented in the library
//! crate. This file provides the hardware-specific initialization that
//! needs the ESP32-S3 HAL, which requires the `esp-rs` toolchain.
//!
//! ## What works (library crate, tested on x86_64):
//! - PN532 SPI protocol driver (`src/nfc/pn532.rs`)
//! - SmartcardDriver implementation for NFC (`src/nfc/pn532_driver.rs`)
//! - CCID core protocol handler (`src/ccid_core.rs`)
//! - NFC device profile and synthetic ATR generation (`src/nfc/mod.rs`)
//!
//! ## What needs the ESP32-S3 HAL (TODO, hardware-dependent):
//! - SPI peripheral initialization
//! - USB OTG device setup (USB device class with CCID endpoints)
//! - GPIO configuration for PN532 CS pin
//! - Main loop: USB polling + NFC card presence monitoring

// ============================================================================
// Conditional compilation: this binary is for ESP32-S3 (Xtensa) target.
// On x86_64 (test/CI), it compiles as a no-op stub.
// ============================================================================

// For ESP32-S3 Xtensa target:
#![cfg_attr(target_arch = "xtensa", no_std)]
#![cfg_attr(target_arch = "xtensa", no_main)]

// ============================================================================
// ESP32-S3 firmware entry point
// ============================================================================
//
// TODO: The implementation below is a structural template showing the
// intended initialization flow. It requires the following ESP32-S3 HAL
// dependencies to be added to Cargo.toml under a target-specific section:
//
// [target.'cfg(target_arch = "xtensa")'.dependencies]
// esp-hal = { version = "0.22", features = ["esp32s3"] }
// esp-backtrace = { version = "0.14", features = ["esp32s3", "panic-handler", "println"] }
// esp-println = { version = "0.12", features = ["esp32s3"] }
// usb-device = "0.3"
//
// The exact versions and features may need adjustment based on the
// esp-rs ecosystem version at the time of implementation.
//
// Once these dependencies are available, uncomment and complete the
// ESP32-S3 specific code below.
//

/*
// ── Uncomment when building with esp-rs toolchain ──────────────────────

use esp_hal::prelude::*;
use esp_hal::gpio::{Io, Level, Output};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::SpiMode;

use ccid_firmware_rs::nfc::{Pn532, Pn532Driver};
use ccid_firmware_rs::nfc::NFC_PROFILE;
use ccid_firmware_rs::ccid_core::CcidMessageHandler;

#[entry]
fn main() -> ! {
    // 1. Initialize ESP32-S3 peripherals
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);

    // 2. Configure SPI for PN532 (Bolty-style pinout)
    let sclk = io.pins.gpio12;
    let mosi = io.pins.gpio11;
    let miso = io.pins.gpio13;
    let cs = Output::new(io.pins.gpio10, Level::High);

    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(1_000_000u32.Hz()) // 1 MHz SPI clock
            .with_mode(SpiMode::Mode0),         // CPOL=0, CPHA=0
    )
    .with_sck(sclk)
    .with_mosi(mosi)
    .with_miso(miso);

    // 3. Initialize PN532 NFC driver
    let pn532 = Pn532::new(spi, cs);
    let mut driver = Pn532Driver::new(pn532);

    // Initialize PN532 (firmware check + SAM config + RF config)
    match driver.init() {
        Ok(()) => esp_println::println!("PN532 initialized successfully"),
        Err(e) => {
            esp_println::println!("PN532 init failed: {:?}", e);
            loop {} // halt on init failure
        }
    }

    // 4. Initialize CCID message handler with NFC driver
    let mut ccid_handler = CcidMessageHandler::new(driver, NFC_PROFILE.vendor_id);

    // 5. Initialize USB OTG device
    //
    // TODO: ESP32-S3 USB OTG setup. The esp-hal USB support is evolving.
    // The approach will be similar to the STM32 path:
    //   - Create a UsbBusAllocator from the USB peripheral
    //   - Allocate CCID bulk IN/OUT and interrupt IN endpoints
    //   - Create a CcidClass-like wrapper (may need adaptation from
    //     src/ccid.rs for the esp-hal USB API)
    //   - Create a UsbDevice with the NFC_PROFILE identity
    //
    // For reference, the STM32 path (src/ccid.rs) wraps CcidMessageHandler
    // with USB endpoint I/O. The same pattern applies here, substituting
    // the ESP32-S3 USB peripheral for the STM32 OTG FS.
    //
    // Pseudo-code:
    //
    //   let usb = Usb::new(peripherals.USB0);
    //   let usb_bus = UsbBusAllocator::new(usb);
    //   let ep_int = usb_bus.interrupt::<In>(8);
    //   let ccid_class = CcidClass::new(&usb_bus, driver, ep_int);
    //   let usb_dev = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(0x1209, 0xCC1D))
    //       .manufacturer(NFC_PROFILE.manufacturer)
    //       .product(NFC_PROFILE.product)
    //       .serial_number(NFC_PROFILE.serial_number)
    //       .device_class(0x00)
    //       .build();

    esp_println::println!("NFC CCID Reader starting main loop...");

    // 6. Main loop: poll USB and monitor NFC card presence
    loop {
        // Poll USB device for host communication
        // TODO: usb_dev.poll(&mut [&mut ccid_class]);

        // Periodically check for NFC card presence changes
        // The CcidMessageHandler.check_card_presence() method handles
        // this and triggers slot change notifications.
        // TODO: if let Some(changed) = ccid_handler.check_card_presence() { ... }

        // Small delay to prevent busy-spinning
        // TODO: Use esp_hal::delay for proper timing
    }
}
*/

// ============================================================================
// Stub main for non-Xtensa targets (CI / test builds)
// ============================================================================
#[cfg(not(target_arch = "xtensa"))]
fn main() {
    // This binary is only meant for ESP32-S3 (Xtensa).
    // On other targets, it compiles as a no-op for CI compatibility.
    println!("ccid-nfc: This binary requires ESP32-S3 hardware.");
    println!("Build with: cargo build --features nfc --no-default-features --target xtensa-esp32s3-none-elf");
    println!("See BUILDING.md for full NFC build instructions.");
}
