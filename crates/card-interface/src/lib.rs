#![no_std]

// Frontend diagnostics trait surface (t_4864e74e) — module doc carries
// the home justification (ccid-service boundary + no_std frontends).
pub mod frontend_diag;

pub use frontend_diag::{FrontendDiagnostics, FrontendErrorKind, FrontendHealth, FrontendStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresenceState {
    pub present: bool,
}

/// Transport-neutral classification of a failed card exchange, the
/// vocabulary the CCID error-map table consumes (issue #136).
///
/// Same design rule as [`FrontendErrorKind`]: driver error enums are
/// backend-specific; this is the coarse common vocabulary the CCID
/// handler layer reads. The mapping class → CCID bError lives in
/// ccid-core (`xfr_failure_b_error`, every entry reference-cited).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XfrErrorKind {
    /// Received byte failed ISO 7816-3 parity — the byte is corrupt,
    /// the exchange cannot be trusted.
    Parity,
    /// Receive overrun — bytes were lost (host too slow / stream
    /// shredded); the stream is desynchronized.
    Overrun,
    /// Card did not answer within the waiting-time budget (BWT-class).
    Timeout,
    /// Card powered but not answering at all (mute) — osmo WTIME_EXP
    /// semantics, where the reader deactivates the card.
    Mute,
    /// Reader-side electrical/driver failure (no card signal class).
    Hardware,
    /// Driver error the backend cannot classify — keeps the historical
    /// fold-everything behavior (see the error-map table).
    Unclassified,
}

pub trait CardBackend {
    type Error: core::fmt::Debug;

    fn power_on(&mut self, atr_buf: &mut [u8]) -> core::result::Result<usize, Self::Error>;
    fn power_off(&mut self);
    fn is_card_present(&mut self) -> bool;
    fn transmit_apdu(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error>;

    /// Classify a failed [`CardBackend::transmit_apdu`] error for the
    /// CCID error-map table (#136). Default: [`XfrErrorKind::Unclassified`]
    /// — backends with a concrete error enum override this so the
    /// reference-correct bError (e.g. parity → 0xFD) reaches the host.
    fn xfr_error_class(&self, _err: &Self::Error) -> XfrErrorKind {
        XfrErrorKind::Unclassified
    }

    /// Count of driver-level reinitializations since boot (self-healing
    /// after consecutive failures), surfaced through diagnostics
    /// (CCID Escape 0xD0). Drivers without self-healing keep the 0.
    fn reinit_count(&self) -> u32 {
        0
    }
}

pub trait ContactCardExt: CardBackend {
    fn transmit_raw(
        &mut self,
        data: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error>;
    fn set_protocol(&mut self, protocol: u8);
    fn get_protocol(&self) -> u8;
    fn set_clock(&mut self, enable: bool);
    fn set_clock_and_rate(
        &mut self,
        clock_hz: u32,
        rate_bps: u32,
    ) -> core::result::Result<(u32, u32), Self::Error>;
}

pub trait NfcCardExt: CardBackend {
    fn init(&mut self) -> core::result::Result<(), Self::Error>;
    fn poll_card_presence(&mut self) -> PresenceState;
    fn session_active(&self) -> bool;

    /// Cached UID of the present card, read during activation
    /// (anticollision). Serves reader-level pseudo-APDUs (PC/SC GET UID)
    /// without card communication. `None` when no UID is known.
    fn card_uid(&self) -> Option<&[u8]> {
        None
    }
}

/// The slot-bookkeeping contract of the shared CCID handler core
/// (`ccid-core` `CcidHandlerCore`, PERF_AND_DRY_STRATEGY §3.1): presence
/// polling, session liveness and the optional cached UID. This is the
/// transport/card-axis-neutral slice of [`NfcCardExt`] — every NFC driver
/// satisfies it via the blanket impl below, and contact drivers
/// (STM32 smartcard USART) implement it directly:
/// `poll_card_presence()` maps to the card-detect GPIO, `session_active()`
/// returns false so the core always polls (contact slots have no
/// activation short-circuit).
pub trait SlotDriver: CardBackend {
    fn poll_card_presence(&mut self) -> PresenceState;
    fn session_active(&self) -> bool;

    /// Cached UID of the present card (contactless anticollision);
    /// `None` on contact drivers — the core's CLA=0xFF pseudo-APDU
    /// interception is disabled for contact reader profiles anyway.
    fn card_uid(&self) -> Option<&[u8]> {
        None
    }
}

impl<D: NfcCardExt> SlotDriver for D {
    fn poll_card_presence(&mut self) -> PresenceState {
        NfcCardExt::poll_card_presence(self)
    }

    fn session_active(&self) -> bool {
        NfcCardExt::session_active(self)
    }

    fn card_uid(&self) -> Option<&[u8]> {
        NfcCardExt::card_uid(self)
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
pub struct MockCardBackend {
    card_present: bool,
    atr: std::vec::Vec<u8>,
    apdu_response: std::vec::Vec<u8>,
    session_active: bool,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockError {
    NoCard,
    NotPowered,
    BufferOverflow,
}

#[cfg(test)]
impl MockCardBackend {
    pub fn new(card_present: bool, atr: &[u8], apdu_response: &[u8]) -> Self {
        Self {
            card_present,
            atr: atr.to_vec(),
            apdu_response: apdu_response.to_vec(),
            session_active: false,
        }
    }

    pub fn set_card_present(&mut self, present: bool) {
        self.card_present = present;
        if !present {
            self.session_active = false;
        }
    }
}

#[cfg(test)]
impl CardBackend for MockCardBackend {
    type Error = MockError;

    fn power_on(&mut self, atr_buf: &mut [u8]) -> core::result::Result<usize, Self::Error> {
        if !self.card_present {
            self.session_active = false;
            return Err(MockError::NoCard);
        }

        if atr_buf.len() < self.atr.len() {
            return Err(MockError::BufferOverflow);
        }

        atr_buf[..self.atr.len()].copy_from_slice(&self.atr);
        self.session_active = true;
        Ok(self.atr.len())
    }

    fn power_off(&mut self) {
        self.session_active = false;
    }

    fn is_card_present(&mut self) -> bool {
        self.card_present
    }

    fn transmit_apdu(
        &mut self,
        _command: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error> {
        if !self.card_present {
            self.session_active = false;
            return Err(MockError::NoCard);
        }

        if !self.session_active {
            return Err(MockError::NotPowered);
        }

        if response.len() < self.apdu_response.len() {
            return Err(MockError::BufferOverflow);
        }

        response[..self.apdu_response.len()].copy_from_slice(&self.apdu_response);
        Ok(self.apdu_response.len())
    }
}

#[cfg(test)]
mod tests {
    use super::{CardBackend, MockCardBackend, MockError, PresenceState, XfrErrorKind};

    #[test]
    fn test_mock_power_on_writes_atr() {
        let atr = [0x3B, 0x80, 0x01, 0x01];
        let mut backend = MockCardBackend::new(true, &atr, &[0x90, 0x00]);
        let mut atr_buf = [0u8; 16];

        let atr_len = backend.power_on(&mut atr_buf).unwrap();

        assert_eq!(atr_len, atr.len());
        assert_eq!(&atr_buf[..atr_len], &atr);
    }

    #[test]
    fn test_mock_power_on_no_card() {
        let mut backend = MockCardBackend::new(false, &[0x3B, 0x00], &[0x90, 0x00]);
        let mut atr_buf = [0u8; 16];

        let result = backend.power_on(&mut atr_buf);

        assert_eq!(result, Err(MockError::NoCard));
    }

    #[test]
    fn test_mock_power_off_clears_session() {
        let mut backend = MockCardBackend::new(true, &[0x3B, 0x00], &[0x90, 0x00]);
        let mut atr_buf = [0u8; 16];

        backend.power_on(&mut atr_buf).unwrap();
        backend.power_off();

        let mut response = [0u8; 8];
        let result = backend.transmit_apdu(&[0x00, 0x84, 0x00, 0x00], &mut response);
        assert_eq!(result, Err(MockError::NotPowered));
    }

    #[test]
    fn test_mock_transmit_apdu_success() {
        let response_bytes = [0x90, 0x00];
        let mut backend = MockCardBackend::new(true, &[0x3B, 0x00], &response_bytes);
        let mut atr_buf = [0u8; 16];
        let mut response = [0u8; 8];

        backend.power_on(&mut atr_buf).unwrap();
        let len = backend
            .transmit_apdu(&[0x00, 0xA4, 0x04, 0x00], &mut response)
            .unwrap();

        assert_eq!(len, response_bytes.len());
        assert_eq!(&response[..len], &response_bytes);
    }

    #[test]
    fn test_mock_transmit_without_power_on() {
        let mut backend = MockCardBackend::new(true, &[0x3B, 0x00], &[0x90, 0x00]);
        let mut response = [0u8; 8];

        let result = backend.transmit_apdu(&[0x00, 0x84, 0x00, 0x00], &mut response);

        assert_eq!(result, Err(MockError::NotPowered));
    }

    #[test]
    fn test_mock_is_card_present() {
        let mut backend = MockCardBackend::new(true, &[0x3B, 0x00], &[0x90, 0x00]);

        assert!(backend.is_card_present());
    }

    #[test]
    fn test_mock_set_card_present() {
        let mut backend = MockCardBackend::new(true, &[0x3B, 0x00], &[0x90, 0x00]);

        backend.set_card_present(false);

        assert!(!backend.is_card_present());
    }

    #[test]
    fn test_presence_state_equality() {
        assert_eq!(
            PresenceState { present: true },
            PresenceState { present: true }
        );
        assert_ne!(
            PresenceState { present: true },
            PresenceState { present: false }
        );
    }

    #[test]
    fn test_xfr_error_default_classification_is_unclassified() {
        // The default CardBackend seam must fold to Unclassified so
        // unmodified backends keep the historical error shape (#136:
        // behavior-preserving default).
        let mut backend = MockCardBackend::new(true, &[0x3B, 0x00], &[0x90, 0x00]);
        let mut response = [0u8; 8];
        let err = backend
            .transmit_apdu(&[0x00, 0x84, 0x00, 0x00], &mut response)
            .unwrap_err();
        assert_eq!(backend.xfr_error_class(&err), XfrErrorKind::Unclassified);
    }

    #[test]
    fn test_xfr_error_kinds_are_distinct() {
        let kinds = [
            XfrErrorKind::Parity,
            XfrErrorKind::Overrun,
            XfrErrorKind::Timeout,
            XfrErrorKind::Mute,
            XfrErrorKind::Hardware,
            XfrErrorKind::Unclassified,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for (j, b) in kinds.iter().enumerate() {
                assert_eq!(a == b, i == j);
            }
        }
    }
}
