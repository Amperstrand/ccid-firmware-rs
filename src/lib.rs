//! CCID Smartcard Reader Firmware Library
//!
//! This library provides the core CCID protocol implementation and PIN pad
//! functionality for the STM32F469-DISCO smartcard reader.
//!
//! Reference: CCID Rev 1.1 Spec (USB-IF DWG_Smart-Card_CCID_Rev110.pdf)
//! Reference: https://ccid.apdu.fr/ccid/section.html (PIN pad reader identities)
//! Reference: PC/SC Part 10 v2.02.09 (Secure PIN Entry)

#![cfg_attr(not(test), no_std)]
#![allow(dead_code)] // PIN pad scaffolding not yet in use
#![allow(unused_imports)] // Public re-exports for future use
#![allow(clippy::identity_op)] // XOR with 0 for LRC clarity
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::manual_is_multiple_of)]

pub mod pcsc10;
pub mod pinpad;
pub mod protocol_unit;

pub use pinpad::PinModifyParams;
#[cfg(feature = "display")]
pub use pinpad::{draw_pinpad, ButtonId, Keypad, TouchHandler};
pub use pinpad::{ApduError, VerifyApduBuilder, VerifyResponse};
pub use pinpad::{PinBuffer, PinEntryContext, PinEntryState, PinResult, PinVerifyParams};

// Re-export PC/SC Part 10 types
pub use pcsc10::{
    FeatureDiscovery, PinProperties, TlvProperties, FEATURE_GET_TLV_PROPERTIES,
    FEATURE_IFD_PIN_PROPERTIES, FEATURE_MODIFY_PIN_DIRECT, FEATURE_VERIFY_PIN_DIRECT,
    TLV_BENTRYVALIDATIONCONDITION, TLV_BMAXPINSIZE, TLV_BMINPINSIZE, TLV_WLCDLAYOUT,
    VALIDATION_KEY, VALIDATION_MAX_LENGTH, VALIDATION_TIMEOUT,
};
