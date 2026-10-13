#![allow(dead_code)]

use crate::turnaround::TurnaroundStats;
pub use card_interface::PresenceState;
use card_interface::{CardBackend, NfcCardExt};

/// NFC driver error types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NfcError {
    /// Driver has not been initialized
    NotInitialized,
    /// No NFC card is present
    NoCard,
    /// Communication error with NFC controller
    CommunicationError,
    /// Operation timed out
    Timeout,
    /// Buffer overflow during data transfer
    BufferOverflow,
}

/// NFC driver trait for smartcard communication
///
/// This trait provides a minimal interface for interacting with NFC controllers
/// like PN532 to communicate with contactless smartcards.
pub trait NfcDriver {
    /// Error type for driver operations
    type Error: core::fmt::Debug;

    /// Initialize the NFC driver
    ///
    /// Must be called before any other operations.
    fn init(&mut self) -> Result<(), Self::Error>;

    /// Check if an NFC card is present
    ///
    /// Returns true if a card is detected in the field.
    fn is_card_present(&mut self) -> bool;

    fn poll_card_presence(&mut self) -> PresenceState {
        PresenceState {
            present: self.is_card_present(),
        }
    }

    fn session_active(&self) -> bool {
        false
    }

    /// Issue #120 decomposition of the last `transmit_apdu`:
    /// `(blocks, wtx_replies, recoveries, max_transceive_us)` — surfaced
    /// through Escape 0xD2 words 4-8. `None` until an instrumented
    /// backend records one (MFRC522 only for now).
    fn exchange_decomposition(&self) -> Option<(u32, u32, u32, u32)> {
        None
    }

    /// Escape 0xD3 probe: total µs for `reads` back-to-back register
    /// reads over the NFC controller bus (the transceive poll-loop
    /// transaction). `None` on backends without a reachable bus.
    fn i2c_timing_probe(&mut self, _reads: u32) -> Option<u32> {
        None
    }

    fn configured_fwt_ms(&self) -> u32 {
        0
    }

    /// Power on the NFC card and read its Answer-to-Reset (ATR)
    ///
    /// # Arguments
    /// * `atr_buf` - Buffer to store the ATR bytes
    ///
    /// # Returns
    /// The number of ATR bytes written to the buffer on success.
    /// The ATR typically ranges from 2 to 33 bytes.
    fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, Self::Error>;

    /// Power off the NFC card
    fn power_off(&mut self);

    /// Transmit an APDU command to the NFC card
    ///
    /// # Arguments
    /// * `command` - The APDU command bytes to send
    /// * `response` - Buffer to store the response
    ///
    /// # Returns
    /// The number of bytes written to the response buffer.
    fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Self::Error>;

    /// Cached UID of the present card, read during activation (anticollision).
    ///
    /// Serves reader-level pseudo-APDUs (PC/SC GET UID) without card
    /// communication. None when no UID is known.
    fn card_uid(&self) -> Option<&[u8]> {
        None
    }

    fn reinit_count(&self) -> u32 {
        0
    }
}

/// Bridges firmware-local [`NfcDriver`] implementations to the shared
/// CCID handler core (ccid-core, PERF_AND_DRY_STRATEGY §3.1), which is
/// generic over the card-interface traits — one adapter instead of
/// touching every driver impl.
///
/// The second field carries the issue #120 turnaround counters (Escape
/// 0xD2): every card exchange passes through here, so this is the
/// single point where the card-I/O half of the latency budget is
/// stamped. The serve-loop half (RX byte → response flushed) is stamped
/// in `main.rs` and written through the wrapper's passthrough.
pub struct CardDriverAdapter<D>(pub D, pub TurnaroundStats);

impl<D: NfcDriver> CardDriverAdapter<D> {
    pub fn new(driver: D) -> Self {
        Self(driver, TurnaroundStats::new())
    }
}

impl<D: NfcDriver> CardBackend for CardDriverAdapter<D> {
    type Error = D::Error;

    fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.power_on(atr_buf)
    }

    fn power_off(&mut self) {
        self.0.power_off()
    }

    fn is_card_present(&mut self) -> bool {
        self.0.is_card_present()
    }

    fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Self::Error> {
        let started_us = crate::t1::now_us();
        let result = self.0.transmit_apdu(command, response);
        // Host builds have no clock (now_us() == 0) — the zero sample is
        // recorded harmlessly and does not disturb a nonzero max.
        let elapsed_us = (crate::t1::now_us() - started_us).clamp(0, u32::MAX as i64) as u32;
        self.1.record_card_exchange_us(elapsed_us);
        if let Some(d) = self.0.exchange_decomposition() {
            self.1.record_exchange_decomposition(d.0, d.1, d.2, d.3);
        }
        result
    }

    fn reinit_count(&self) -> u32 {
        self.0.reinit_count()
    }
}

impl<D: NfcDriver> NfcCardExt for CardDriverAdapter<D> {
    fn init(&mut self) -> Result<(), Self::Error> {
        self.0.init()
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        self.0.poll_card_presence()
    }

    fn session_active(&self) -> bool {
        self.0.session_active()
    }

    fn card_uid(&self) -> Option<&[u8]> {
        self.0.card_uid()
    }
}

/// Mock NFC driver for testing
///
/// This mock driver simulates an NFC controller for host-side testing.
/// It allows configuration of card presence, ATR, and APDU responses.
#[derive(Debug, Clone)]
pub struct MockNfcDriver {
    /// Whether a card is present
    card_present: bool,
    /// ATR bytes to return when card is powered on
    atr: Vec<u8>,
    /// APDU response to return for any transmit request
    apdu_response: Vec<u8>,
    /// Whether the driver has been initialized
    initialized: bool,
    session_active: bool,
    /// UID returned by card_uid() (empty = no UID known)
    uid: Vec<u8>,
    /// Test instrumentation: number of is_card_present() calls.
    poll_count: u32,
    /// Test instrumentation: last APDU handed to transmit_apdu.
    last_apdu: Vec<u8>,
}

impl MockNfcDriver {
    /// Create a new mock NFC driver
    ///
    /// # Arguments
    /// * `card_present` - Whether to simulate a card being present
    /// * `atr` - ATR bytes to return on power_on
    /// * `apdu_response` - Default APDU response bytes
    pub fn new(card_present: bool, atr: &[u8], apdu_response: &[u8]) -> Self {
        Self {
            card_present,
            atr: atr.to_vec(),
            apdu_response: apdu_response.to_vec(),
            initialized: false,
            session_active: false,
            uid: Vec::new(),
            poll_count: 0,
            last_apdu: Vec::new(),
        }
    }

    /// Test instrumentation: number of presence polls performed.
    pub fn poll_count(&self) -> u32 {
        self.poll_count
    }

    /// Test instrumentation: the last APDU forwarded to the card.
    pub fn last_apdu(&self) -> &[u8] {
        &self.last_apdu
    }

    pub fn set_uid(&mut self, uid: &[u8]) {
        self.uid = uid.to_vec();
    }

    #[cfg(test)]
    pub fn set_card_present(&mut self, card_present: bool) {
        self.card_present = card_present;
        if !card_present {
            self.session_active = false;
        }
    }
}

impl NfcDriver for MockNfcDriver {
    type Error = NfcError;

    fn init(&mut self) -> Result<(), Self::Error> {
        self.initialized = true;
        Ok(())
    }

    fn is_card_present(&mut self) -> bool {
        self.poll_count += 1;
        self.card_present
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        let present = self.is_card_present();
        if !present {
            self.session_active = false;
        }
        PresenceState { present }
    }

    fn session_active(&self) -> bool {
        self.session_active
    }

    fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
        if !self.card_present {
            self.session_active = false;
            return Err(NfcError::NoCard);
        }

        if atr_buf.len() < self.atr.len() {
            return Err(NfcError::BufferOverflow);
        }

        atr_buf[..self.atr.len()].copy_from_slice(&self.atr);
        self.session_active = true;
        Ok(self.atr.len())
    }

    fn power_off(&mut self) {
        self.session_active = false;
    }

    fn card_uid(&self) -> Option<&[u8]> {
        (!self.uid.is_empty()).then_some(&self.uid)
    }

    fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Self::Error> {
        if !self.card_present {
            self.session_active = false;
            return Err(NfcError::NoCard);
        }

        if !self.session_active {
            return Err(NfcError::NotInitialized);
        }

        self.last_apdu = command.to_vec();

        if response.len() < self.apdu_response.len() {
            return Err(NfcError::BufferOverflow);
        }

        response[..self.apdu_response.len()].copy_from_slice(&self.apdu_response);
        Ok(self.apdu_response.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_card_present() {
        let atr = vec![0x3B, 0x80, 0x01, 0x01]; // Sample ATR
        let apdu_response = vec![0x90, 0x00]; // Success SW

        let mut driver = MockNfcDriver::new(true, &atr, &apdu_response);

        // Initialize the driver
        driver.init().unwrap();

        // Verify card is present
        assert!(driver.is_card_present());
        assert_eq!(driver.poll_card_presence(), PresenceState { present: true });

        // Power on and check ATR
        let mut atr_buf = [0u8; 64];
        let atr_len = driver.power_on(&mut atr_buf).unwrap();
        assert_eq!(atr_len, atr.len());
        assert_eq!(&atr_buf[..atr_len], &atr);

        // Power off
        driver.power_off();
    }

    #[test]
    fn test_mock_no_card() {
        let atr = vec![0x3B, 0x80, 0x01, 0x01];
        let apdu_response = vec![0x90, 0x00];

        let mut driver = MockNfcDriver::new(false, &atr, &apdu_response);

        // Initialize the driver
        driver.init().unwrap();

        // Verify card is not present
        assert!(!driver.is_card_present());
        assert_eq!(
            driver.poll_card_presence(),
            PresenceState { present: false }
        );

        // Power on should return NoCard error
        let mut atr_buf = [0u8; 64];
        let result = driver.power_on(&mut atr_buf);
        assert!(matches!(result, Err(NfcError::NoCard)));
    }

    #[test]
    fn test_mock_transmit_apdu() {
        let atr = vec![0x3B, 0x80, 0x01, 0x01];
        let apdu_response = vec![0x90, 0x00]; // Success SW

        let mut driver = MockNfcDriver::new(true, &atr, &apdu_response);

        // Initialize and power on
        driver.init().unwrap();
        let mut atr_buf = [0u8; 64];
        driver.power_on(&mut atr_buf).unwrap();

        // Transmit APDU
        let command = vec![0x00, 0xA4, 0x04, 0x00]; // SELECT command
        let mut response_buf = [0u8; 256];
        let response_len = driver.transmit_apdu(&command, &mut response_buf).unwrap();

        assert_eq!(response_len, apdu_response.len());
        assert_eq!(&response_buf[..response_len], &apdu_response);
    }

    #[test]
    fn test_mock_transmit_no_card() {
        let atr = vec![0x3B, 0x80, 0x01, 0x01];
        let apdu_response = vec![0x90, 0x00];

        let mut driver = MockNfcDriver::new(false, &atr, &apdu_response);

        // Initialize
        driver.init().unwrap();

        // Transmit APDU should return NoCard error
        let command = vec![0x00, 0xA4, 0x04, 0x00];
        let mut response_buf = [0u8; 256];
        let result = driver.transmit_apdu(&command, &mut response_buf);
        assert!(matches!(result, Err(NfcError::NoCard)));
    }

    #[test]
    fn test_mock_transmit_requires_power_on() {
        let atr = vec![0x3B, 0x80, 0x01, 0x01];
        let apdu_response = vec![0x90, 0x00];

        let mut driver = MockNfcDriver::new(true, &atr, &apdu_response);
        driver.init().unwrap();

        let mut response_buf = [0u8; 256];
        let result = driver.transmit_apdu(&[0x00, 0x84, 0x00, 0x00], &mut response_buf);
        assert!(matches!(result, Err(NfcError::NotInitialized)));
    }

    #[test]
    fn test_transmit_without_session_fails() {
        let atr = vec![0x3B, 0x80, 0x01, 0x01];
        let apdu_response = vec![0x90, 0x00];

        let mut driver = MockNfcDriver::new(true, &atr, &apdu_response);
        driver.init().unwrap();

        let mut response_buf = [0u8; 256];
        let result = driver.transmit_apdu(&[0x00, 0x84, 0x00, 0x00], &mut response_buf);
        assert!(matches!(result, Err(NfcError::NotInitialized)));
    }

    #[test]
    fn test_mock_session_active_tracks_power_state() {
        let atr = vec![0x3B, 0x80, 0x01, 0x01];
        let apdu_response = vec![0x90, 0x00];

        let mut driver = MockNfcDriver::new(true, &atr, &apdu_response);
        driver.init().unwrap();
        assert!(!driver.session_active());

        let mut atr_buf = [0u8; 64];
        driver.power_on(&mut atr_buf).unwrap();
        assert!(driver.session_active());

        driver.power_off();
        assert!(!driver.session_active());
    }
}
