#![allow(dead_code)]

use crate::turnaround::TurnaroundStats;
pub use card_interface::PresenceState;
use card_interface::{
    CardBackend, FrontendDiagnostics, FrontendErrorKind, FrontendHealth, FrontendStatus, NfcCardExt,
};

/// Maps the shared NFC driver error vocabulary onto the board-free
/// frontend diagnostics classification (t_4864e74e).
impl From<NfcError> for FrontendErrorKind {
    fn from(e: NfcError) -> Self {
        match e {
            NfcError::NotInitialized => FrontendErrorKind::NotInitialized,
            NfcError::NoCard => FrontendErrorKind::NoCard,
            NfcError::CommunicationError => FrontendErrorKind::Bus,
            NfcError::Timeout => FrontendErrorKind::Timeout,
            NfcError::BufferOverflow => FrontendErrorKind::BufferOverflow,
        }
    }
}

/// NFC driver error types (#257 error-model pass: `Display`/
/// `core::error::Error` derived from the variant docs — additive, no
/// variant or behavior change; the twin-collapse itself stays
/// P5-sequenced).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NfcError {
    /// Driver has not been initialized
    #[error("driver not initialized")]
    NotInitialized,
    /// No NFC card is present
    #[error("no NFC card present")]
    NoCard,
    /// Communication error with NFC controller
    #[error("communication error with the NFC controller")]
    CommunicationError,
    /// Operation timed out
    #[error("operation timed out")]
    Timeout,
    /// Buffer overflow during data transfer
    #[error("buffer overflow during data transfer")]
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

    /// #328/#332: time-driven maintenance during host-silent periods
    /// (serve-loop idle wake). Default no-op — drivers whose presence
    /// model depends on host poll cadence override it (PN7160: the
    /// parked re-arm).
    fn poll_maintenance(&mut self, _now_ms: u64) {}

    /// Init routed through the backend's recovery machinery when it has
    /// one (MFRC522: the failure tracker + threshold-triggered
    /// `full_reinit`, Codex reviews on #43/#40/#38). Retry paths — the
    /// `RetryFrontend` degraded-retry loop above all — must call this
    /// so a backend needing the hardware reset actually gets it; a
    /// plain `init()` cannot trigger the self-healing path.
    fn init_recovering(&mut self) -> Result<(), Self::Error> {
        self.init()
    }

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

    /// #360 P1 (Codex 2026-10-21, merged-head follow-up): does
    /// `is_card_present()` perform a REAL physical liveness check
    /// during an active session? Backends that short-circuit to
    /// `present: true` for any `ActiveSession` without touching
    /// hardware (MFRC522's `poll_physical_card`) must keep the
    /// default `false` — the vpcd cached-ATR fast path is only sound
    /// when card removal is observable between APDUs. Such backends
    /// keep the per-probe `power_on` behavior (reactivates the
    /// frontend, eventually notices the missing card).
    fn presence_poll_detects_removal(&self) -> bool {
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

    /// Escape 0xD3 append (#519): raw timer registers
    /// `[TMode, TPrescaler, TReloadHi, TReloadLo]` READ BACK from the
    /// controller — the hardware arming, unlike `configured_fwt_ms`
    /// (the requested-timeout cache, which both pre- and post-#464
    /// firmware generations fill with the FWI-derived request). `None`
    /// on backends without a readable hardware timer (everything but
    /// MFRC522): the 0xD3 payload stays the legacy 12 bytes.
    fn timer_registers(&mut self) -> Option<[u8; 4]> {
        None
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

    /// WTX advisory counters (issue #156 A3, Escape 0xD4):
    /// `(last exchange, lifetime total)` PN7160 RF_WTX notifications
    /// (`6F 17 00`) consumed during DATA collection. `None` on
    /// backends without a WTX-reporting NFC controller (MFRC522/PN532).
    fn wtx_counters(&self) -> Option<(u32, u32)> {
        None
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

impl<D: NfcDriver + FrontendDiagnostics> FrontendDiagnostics for CardDriverAdapter<D> {
    fn frontend_status(&self) -> FrontendStatus {
        self.0.frontend_status()
    }

    fn frontend_self_test(&mut self) -> Result<(), FrontendErrorKind> {
        self.0.frontend_self_test()
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
    /// Test instrumentation: WTX counters surfaced by wtx_counters()
    /// (issue #156 A3 / Escape 0xD4).
    wtx: Option<(u32, u32)>,
    /// Test instrumentation: raw timer registers served by
    /// `timer_registers()` (#519, Escape 0xD3 append).
    timer_regs: Option<[u8; 4]>,
    /// Test instrumentation: invoked at the START of transmit_apdu with
    /// a nominal stall elapsed (issue #156 B) — models the PN7160
    /// driver's stall hook firing from inside a blocking exchange.
    stall_probe: Option<fn(u32)>,
    /// Test instrumentation (t_4864e74e): status served by
    /// `frontend_status()` — lets host tests drive the health-transition
    /// logging path exactly like a real degraded frontend would.
    frontend_status: FrontendStatus,
    /// Test instrumentation: outcome served by `frontend_self_test()`.
    self_test_result: Result<(), FrontendErrorKind>,
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
            wtx: None,
            timer_regs: None,
            stall_probe: None,
            frontend_status: FrontendStatus {
                health: FrontendHealth::Healthy,
                ..FrontendStatus::ok()
            },
            self_test_result: Ok(()),
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

    /// Test instrumentation: values returned by `wtx_counters()`.
    pub fn set_wtx_counters(&mut self, last: u32, total: u32) {
        self.wtx = Some((last, total));
    }

    /// Test instrumentation: raw timer readback for the 0xD3 append.
    pub fn set_timer_registers(&mut self, regs: [u8; 4]) {
        self.timer_regs = Some(regs);
    }

    /// Test instrumentation: stall probe invoked inside transmit_apdu.
    pub fn set_stall_probe(&mut self, probe: Option<fn(u32)>) {
        self.stall_probe = probe;
    }

    /// Test instrumentation (t_4864e74e): status served by
    /// `frontend_status()` (presence polls stay counted live).
    pub fn set_frontend_status(&mut self, status: FrontendStatus) {
        self.frontend_status = status;
    }

    /// Test instrumentation: outcome served by `frontend_self_test()`.
    pub fn set_self_test_result(&mut self, result: Result<(), FrontendErrorKind>) {
        self.self_test_result = result;
    }

    // Cross-crate test instrumentation (t_72e1e2ed): esp32-ccid's and
    // ccid-service's own tests both drive the mock — a cfg(test) gate
    // here hides it from downstream test builds, so it stays public.
    pub fn set_card_present(&mut self, card_present: bool) {
        self.card_present = card_present;
        if !card_present {
            self.session_active = false;
        }
    }

    /// Test instrumentation (#444): model a driver that tears down its
    /// active session MID-EXCHANGE (the RF error path: session cleared,
    /// lifecycle parked inactive) while the card STAYS coupled —
    /// presence does not flap. `set_card_present(false)` is the wrong
    /// shape for that scenario.
    pub fn set_session_active(&mut self, active: bool) {
        self.session_active = active;
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
        if let Some(probe) = self.stall_probe {
            probe(1600);
        }
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

    fn wtx_counters(&self) -> Option<(u32, u32)> {
        self.wtx
    }

    fn timer_registers(&mut self) -> Option<[u8; 4]> {
        self.timer_regs
    }
}

impl FrontendDiagnostics for MockNfcDriver {
    fn frontend_status(&self) -> FrontendStatus {
        // Presence polls are served live (the real counter the 0xD0
        // path cares about); the rest is the scripted snapshot.
        FrontendStatus {
            presence_polls: self.poll_count,
            ..self.frontend_status
        }
    }

    fn frontend_self_test(&mut self) -> Result<(), FrontendErrorKind> {
        self.self_test_result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #257 error-model pass lock: the additive Display strings,
    /// byte-locked (the twins themselves stay P5-sequenced).
    #[test]
    fn nfc_error_display_strings_are_locked() {
        assert_eq!(
            NfcError::NotInitialized.to_string(),
            "driver not initialized"
        );
        assert_eq!(NfcError::NoCard.to_string(), "no NFC card present");
        assert_eq!(
            NfcError::CommunicationError.to_string(),
            "communication error with the NFC controller"
        );
        assert_eq!(NfcError::Timeout.to_string(), "operation timed out");
        assert_eq!(
            NfcError::BufferOverflow.to_string(),
            "buffer overflow during data transfer"
        );
        let _: &dyn core::error::Error = &NfcError::Timeout;
    }

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
