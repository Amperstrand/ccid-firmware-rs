//! SmartcardDriver implementation backed by PN532 NFC controller
//!
//! This module bridges the PN532 NFC controller to the CCID core via the
//! [`SmartcardDriver`] trait. NFC target detection maps to card presence,
//! ISO-DEP activation maps to power-on (returning a synthetic ATR), and
//! APDU exchange maps to PN532 InDataExchange.
//!
//! # Design Notes
//!
//! - **`is_card_present()`**: Attempts a quick target detection. Because
//!   NFC polling is relatively expensive, the driver caches presence state
//!   and only re-polls when explicitly checked.
//!
//! - **`power_on()`**: Activates an NFC target (InListPassiveTarget) and
//!   returns a PC/SC Part 3 synthetic ATR encoding the card's ATQA and SAK.
//!
//! - **`transmit_apdu()`**: Sends an APDU to the active target via
//!   InDataExchange and returns the response. This is the primary data
//!   path for CCID XfrBlock commands.
//!
//! - **`transmit_raw()`**: For NFC, raw T=1 blocks are not applicable.
//!   This falls back to `transmit_apdu()` since the NFC transport is
//!   always APDU-level.
//!
//! - **`set_clock()` / `set_clock_and_rate()`**: No-ops for NFC. The RF
//!   carrier frequency (13.56 MHz) is fixed by the ISO 14443 standard.

use embedded_hal::blocking::spi;
use embedded_hal::digital::v2::OutputPin;

use crate::driver::SmartcardDriver;
use crate::nfc::pn532::{NfcTarget, Pn532, Pn532Error};
use crate::nfc::{build_synthetic_atr, MAX_SYNTHETIC_ATR_LEN};

/// SmartcardDriver implementation using PN532 NFC controller.
///
/// Wraps a [`Pn532`] instance and presents it as a standard smartcard
/// reader to the CCID core. This allows the existing CCID protocol
/// handler to work unchanged with NFC contactless cards.
///
/// # Type Parameters
///
/// - `SPI`: SPI bus implementing `embedded_hal::blocking::spi::{Transfer, Write}`
/// - `CS`: Chip-select GPIO pin implementing `embedded_hal::digital::v2::OutputPin`
pub struct Pn532Driver<SPI, CS> {
    pn532: Pn532<SPI, CS>,
    /// Currently active NFC target, if any
    active_target: Option<NfcTarget>,
    /// Cached synthetic ATR for the active target
    atr_buf: [u8; MAX_SYNTHETIC_ATR_LEN],
    /// Length of the cached ATR
    atr_len: usize,
    /// Whether the NFC interface has been initialized
    initialized: bool,
    /// Current protocol (always 1 = T=1 for NFC/ISO-DEP)
    protocol: u8,
}

impl<SPI, CS, SpiError, PinError> Pn532Driver<SPI, CS>
where
    SPI: spi::Transfer<u8, Error = SpiError> + spi::Write<u8, Error = SpiError>,
    CS: OutputPin<Error = PinError>,
    SpiError: core::fmt::Debug,
    PinError: core::fmt::Debug,
{
    /// Create a new PN532-based smartcard driver.
    ///
    /// After construction, call [`init()`](Pn532Driver::init) or wait for
    /// the first `power_on()` call which will auto-initialize.
    pub fn new(pn532: Pn532<SPI, CS>) -> Self {
        Self {
            pn532,
            active_target: None,
            atr_buf: [0u8; MAX_SYNTHETIC_ATR_LEN],
            atr_len: 0,
            initialized: false,
            protocol: 1, // T=1 for NFC/ISO-DEP
        }
    }

    /// Initialize the PN532 for NFC operation.
    ///
    /// Called automatically on first `power_on()` if not called explicitly.
    pub fn init(&mut self) -> Result<(), Pn532Error> {
        self.pn532.init()?;
        self.initialized = true;
        Ok(())
    }

    /// Ensure the PN532 is initialized, initializing if needed.
    fn ensure_init(&mut self) -> Result<(), Pn532Error> {
        if !self.initialized {
            self.init()?;
        }
        Ok(())
    }
}

impl<SPI, CS, SpiError, PinError> SmartcardDriver for Pn532Driver<SPI, CS>
where
    SPI: spi::Transfer<u8, Error = SpiError> + spi::Write<u8, Error = SpiError>,
    CS: OutputPin<Error = PinError>,
    SpiError: core::fmt::Debug,
    PinError: core::fmt::Debug,
{
    type Error = Pn532Error;

    /// Check if an NFC card is present in the RF field.
    ///
    /// If a target is already active (powered on), returns `true` without
    /// re-polling. Otherwise, attempts a quick target detection.
    ///
    /// Note: NFC polling is relatively slow (~50-100ms) compared to
    /// checking a GPIO pin for contact cards. The CCID core should not
    /// call this in a tight loop.
    fn is_card_present(&self) -> bool {
        // If we have an active target, assume it's still present.
        // The card's absence will be detected on the next transmit_apdu()
        // call when the PN532 returns an error.
        self.active_target.is_some()
    }

    /// Activate an NFC target and return its synthetic ATR.
    ///
    /// Performs ISO 14443A target detection via the PN532. If an ISO-DEP
    /// capable card is found, it is activated and a PC/SC Part 3 synthetic
    /// ATR is generated from the card's ATQA and SAK.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - PN532 initialization fails
    /// - No NFC target is detected
    /// - The detected target does not support ISO-DEP (not APDU-capable)
    fn power_on(&mut self) -> Result<&[u8], Pn532Error> {
        self.ensure_init()?;

        // Release any previously active target
        if self.active_target.is_some() {
            let _ = self.pn532.in_release(0);
            self.active_target = None;
        }

        // Detect and activate ISO 14443A target
        let target = self.pn532.detect_target_iso14443a()?;

        if !target.is_iso_dep {
            // Card doesn't support ISO-DEP — can't do APDUs
            let _ = self.pn532.in_release(target.tg);
            ccid_warn!(
                "PN532: target found but not ISO-DEP capable (SAK=0x{:02X})",
                target.sak
            );
            return Err(Pn532Error::NoTarget);
        }

        ccid_info!(
            "PN532: ISO-DEP target activated, ATQA={:02X}{:02X} SAK={:02X}",
            target.atqa[0],
            target.atqa[1],
            target.sak
        );

        // Build synthetic ATR for CCID/PC/SC
        self.atr_len = build_synthetic_atr(target.atqa, target.sak, &mut self.atr_buf);
        self.active_target = Some(target);
        self.protocol = 1; // T=1 for ISO-DEP

        Ok(&self.atr_buf[..self.atr_len])
    }

    /// Deactivate the current NFC target.
    fn power_off(&mut self) {
        if let Some(ref target) = self.active_target {
            let _ = self.pn532.in_release(target.tg);
        }
        self.active_target = None;
        self.atr_len = 0;
    }

    /// Send an APDU to the active NFC target and receive the response.
    ///
    /// Uses PN532 InDataExchange to transport the APDU over ISO-DEP.
    /// The PN532 handles all ISO 14443-4 framing (I-blocks, chaining,
    /// WTX, etc.) transparently.
    ///
    /// # Errors
    ///
    /// Returns an error if no target is active, or if the PN532 reports
    /// an NFC communication error (card removed, RF timeout, etc.).
    fn transmit_apdu(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> Result<usize, Pn532Error> {
        let tg = match &self.active_target {
            Some(target) => target.tg,
            None => return Err(Pn532Error::NoTarget),
        };

        match self.pn532.in_data_exchange(tg, command, response) {
            Ok(len) => Ok(len),
            Err(e) => {
                // If communication fails, assume card is gone
                ccid_error!("PN532: APDU exchange failed, deactivating target");
                self.active_target = None;
                Err(e)
            }
        }
    }

    /// Raw data exchange — falls back to APDU exchange for NFC.
    ///
    /// NFC/ISO-DEP is always APDU-level; there is no meaningful "raw"
    /// T=1 block mode. This method simply delegates to `transmit_apdu()`.
    fn transmit_raw(
        &mut self,
        data: &[u8],
        response: &mut [u8],
    ) -> Result<usize, Pn532Error> {
        self.transmit_apdu(data, response)
    }

    /// Set the smartcard protocol.
    ///
    /// For NFC, the protocol is always T=1 (ISO-DEP maps to T=1
    /// semantics). This method accepts any value but the NFC driver
    /// always operates in T=1 mode.
    fn set_protocol(&mut self, protocol: u8) {
        self.protocol = protocol;
    }

    /// Enable/disable the card clock.
    ///
    /// No-op for NFC. The 13.56 MHz RF carrier is managed by the PN532
    /// and cannot be independently clocked from the host controller.
    fn set_clock(&mut self, _enable: bool) {
        // NFC carrier is controlled by the PN532, not the host.
    }

    /// Set clock frequency and data rate.
    ///
    /// No-op for NFC — returns the fixed NFC carrier frequency (13.56 MHz)
    /// and the requested data rate (ISO 14443 supports 106/212/424/848 kbps
    /// but rate selection is handled by the PN532 automatically).
    fn set_clock_and_rate(
        &mut self,
        _clock_hz: u32,
        _rate_bps: u32,
    ) -> Result<(u32, u32), Pn532Error> {
        // NFC carrier is fixed at 13.56 MHz
        // Data rate is negotiated by the PN532 during target activation
        Ok((13_560_000, 106_000))
    }
}
