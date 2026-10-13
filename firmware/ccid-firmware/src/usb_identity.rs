#![cfg(all(target_arch = "arm", target_os = "none"))]
//! USB Identity Configuration
//!
//! Re-exports USB identity constants from the active device profile.
//! This module provides backward compatibility for code that expects
//! these constants directly.

use ccid_firmware_rs::device_profile::CURRENT_PROFILE;

/// USB Serial Number: profile serial, except the F746 build carries the
/// bench-distinct ST2XXX-F746 (see device_profile::USB_SERIAL_NUMBER).
pub use ccid_firmware_rs::device_profile::USB_SERIAL_NUMBER;

/// USB Vendor ID from active device profile
pub const USB_VENDOR_ID: u16 = CURRENT_PROFILE.vendor_id;

/// USB Product ID from active device profile
pub const USB_PRODUCT_ID: u16 = CURRENT_PROFILE.product_id;

/// USB Manufacturer string from active device profile
pub const USB_MANUFACTURER: &str = CURRENT_PROFILE.manufacturer;

/// USB Product string from active device profile
pub const USB_PRODUCT: &str = CURRENT_PROFILE.product;
