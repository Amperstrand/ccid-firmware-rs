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

/// Shared deterministic PRNG for the fuzz-style "random bytes must not
/// panic" tests in this crate (zero dependencies, reproducible seeds).
#[cfg(test)]
pub(crate) mod testutil {
    pub struct Xorshift32(pub u32);

    impl Xorshift32 {
        pub fn next_u32(&mut self) -> u32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.0 = x;
            x
        }

        pub fn next_u8(&mut self) -> u8 {
            self.next_u32() as u8
        }

        pub fn fill_bytes(&mut self, buf: &mut [u8]) {
            for b in buf.iter_mut() {
                *b = self.next_u8();
            }
        }
    }
}
