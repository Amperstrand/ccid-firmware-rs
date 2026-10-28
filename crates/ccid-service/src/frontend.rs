//! NFC frontend fault isolation (#218 — audit-wave4 intent, reworked
//! onto main's architecture per DECISION_REGISTER D-8).
//!
//! The classic mains historically halted the whole CCID service when
//! the NFC controller failed to initialize — reproducing the
//! commercial-reader failure mode where a wedged frontend takes the
//! reader off the bus entirely. These wrappers keep the host-facing
//! service alive instead:
//!
//! - [`RetryFrontend`] wraps a constructed driver whose `init()` failed
//!   (at boot or later): presence polls return absent AND retry
//!   `init()` — the serve loop already interval-gates presence polls,
//!   so retries ride that cadence for free. A successful retry flips
//!   the frontend back to healthy. Degraded frontends gate the
//!   bus-probe diagnostics (`exchange_decomposition`,
//!   `i2c_timing_probe`, `configured_fwt_ms`) — a dead controller has
//!   no bus to probe.
//! - [`UnavailableNfcDriver`] is the stand-in when the controller
//!   cannot even be constructed (the MFRC522 hw-init failure): the
//!   CCID service still enumerates and answers GetSlotStatus
//!   (card-absent) and Escape 0xD0, while power-on/APDU fail cleanly.
//!   It can never recover in place — the bus reconstruction is not
//!   reachable through the `NfcDriver` trait — but serving degraded
//!   beats a full-service wedge (a reseat still needs a reboot, same
//!   as today's halt loop, minus the dead reader).
//!
//! Power-on / APDU delegate to the inner driver, whose
//! not-initialized guards fail them cleanly. All host-testable.

use crate::nfc::{NfcDriver, NfcError, PresenceState};
use card_interface::{FrontendDiagnostics, FrontendErrorKind, FrontendHealth, FrontendStatus};

/// Degradable wrapper around one NFC frontend.
///
/// Construct via [`RetryFrontend::healthy`] (boot init succeeded) or
/// [`RetryFrontend::degraded`] (boot init failed — serve card-absent
/// and retry `init()` on every presence poll until it succeeds).
pub struct RetryFrontend<D: NfcDriver> {
    inner: D,
    degraded: bool,
}

impl<D: NfcDriver> RetryFrontend<D> {
    /// Wrap a frontend whose boot-time init succeeded.
    pub fn healthy(inner: D) -> Self {
        Self {
            inner,
            degraded: false,
        }
    }

    /// Wrap a frontend whose boot-time init failed — serve degraded and
    /// retry `init()` from the first presence poll.
    pub fn degraded(inner: D) -> Self {
        Self {
            inner,
            degraded: true,
        }
    }

    /// Whether the frontend is currently degraded (init failing).
    pub fn is_degraded(&self) -> bool {
        self.degraded
    }

    /// Unwrap into the inner driver.
    pub fn into_inner(self) -> D {
        self.inner
    }
}

impl<D: NfcDriver> NfcDriver for RetryFrontend<D> {
    fn poll_maintenance(&mut self, now_ms: u64) {
        self.inner.poll_maintenance(now_ms);
    }
    type Error = D::Error;

    fn init(&mut self) -> Result<(), Self::Error> {
        let result = self.inner.init_recovering();
        self.degraded = result.is_err();
        result
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        if self.degraded && self.inner.init_recovering().is_ok() {
            self.degraded = false;
        }
        if self.degraded {
            PresenceState { present: false }
        } else {
            self.inner.poll_card_presence()
        }
    }

    fn is_card_present(&mut self) -> bool {
        self.poll_card_presence().present
    }

    fn session_active(&self) -> bool {
        !self.degraded && self.inner.session_active()
    }

    fn presence_poll_detects_removal(&self) -> bool {
        // #369 P2: delegate — a wrapped PN7160 must keep its fast path.
        !self.degraded && self.inner.presence_poll_detects_removal()
    }

    fn exchange_decomposition(&self) -> Option<(u32, u32, u32, u32)> {
        (!self.degraded)
            .then(|| self.inner.exchange_decomposition())
            .flatten()
    }

    fn i2c_timing_probe(&mut self, reads: u32) -> Option<u32> {
        if self.degraded {
            None
        } else {
            self.inner.i2c_timing_probe(reads)
        }
    }

    fn configured_fwt_ms(&self) -> u32 {
        if self.degraded {
            0
        } else {
            self.inner.configured_fwt_ms()
        }
    }

    fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.inner.power_on(atr_buf)
    }

    fn power_off(&mut self) {
        self.inner.power_off()
    }

    fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Self::Error> {
        self.inner.transmit_apdu(command, response)
    }

    fn card_uid(&self) -> Option<&[u8]> {
        if self.degraded {
            None
        } else {
            self.inner.card_uid()
        }
    }

    fn reinit_count(&self) -> u32 {
        self.inner.reinit_count()
    }

    fn wtx_counters(&self) -> Option<(u32, u32)> {
        if self.degraded {
            None
        } else {
            self.inner.wtx_counters()
        }
    }
}

/// Health surface (t_4864e74e): the wrapper owns the degraded bit, so
/// it classifies — the counters stay the inner frontend's own.
impl<D: NfcDriver + FrontendDiagnostics> FrontendDiagnostics for RetryFrontend<D> {
    fn frontend_status(&self) -> FrontendStatus {
        let mut status = self.inner.frontend_status();
        // Classification rules: the inner frontend is authoritative
        // about Unavailable (it could not be constructed — no amount of
        // init retrying changes that class). Otherwise the wrapper owns
        // the degraded bit: it downgrades a Healthy snapshot while
        // init is failing and upgrades a stale Degraded snapshot once
        // a retry succeeded.
        if status.health != FrontendHealth::Unavailable {
            status.health = if self.degraded {
                FrontendHealth::Degraded
            } else {
                FrontendHealth::Healthy
            };
        }
        status
    }

    fn frontend_self_test(&mut self) -> Result<(), FrontendErrorKind> {
        self.inner.frontend_self_test()
    }
}

/// Stand-in driver when the NFC controller cannot even be constructed:
/// the CCID service stays up — GetSlotStatus answers card-absent,
/// diagnostics counters respond — while power-on/APDU fail cleanly.
/// `init()` always fails, so a [`RetryFrontend`] around it stays
/// degraded until reboot (the bus reconstruction lives in the mains,
/// not behind the `NfcDriver` trait).
pub struct UnavailableNfcDriver;

impl NfcDriver for UnavailableNfcDriver {
    type Error = NfcError;

    fn init(&mut self) -> Result<(), Self::Error> {
        Err(NfcError::NotInitialized)
    }

    fn is_card_present(&mut self) -> bool {
        false
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        PresenceState { present: false }
    }

    fn power_on(&mut self, _atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
        Err(NfcError::NotInitialized)
    }

    fn power_off(&mut self) {}

    fn transmit_apdu(
        &mut self,
        _command: &[u8],
        _response: &mut [u8],
    ) -> Result<usize, Self::Error> {
        Err(NfcError::NotInitialized)
    }
}

/// Never constructed ⇒ permanently [`FrontendHealth::Unavailable`]
/// (t_4864e74e). The self-test cannot pass by construction.
impl FrontendDiagnostics for UnavailableNfcDriver {
    fn frontend_status(&self) -> FrontendStatus {
        FrontendStatus {
            health: FrontendHealth::Unavailable,
            last_error: FrontendErrorKind::NotInitialized,
            ..FrontendStatus::ok()
        }
    }

    fn frontend_self_test(&mut self) -> Result<(), FrontendErrorKind> {
        Err(FrontendErrorKind::NotInitialized)
    }
}

/// One of two concrete frontends sharing an error type — lets a main
/// keep serving when the real driver could not even be constructed
/// (the `Mfrc522<_, Initialized>` typestate has no value on hw-init
/// failure), without dyn dispatch: the primary arm carries the wrapped
/// real driver, the fallback arm the [`UnavailableNfcDriver`] stand-in.
pub enum EitherFrontend<A: NfcDriver, B: NfcDriver<Error = A::Error>> {
    /// The real (RetryFrontend-wrapped) driver.
    Primary(A),
    /// The stand-in frontend.
    Fallback(B),
}

impl<A: NfcDriver, B: NfcDriver<Error = A::Error>> NfcDriver for EitherFrontend<A, B> {
    fn poll_maintenance(&mut self, now_ms: u64) {
        match self {
            Self::Primary(d) => d.poll_maintenance(now_ms),
            Self::Fallback(d) => d.poll_maintenance(now_ms),
        }
    }
    type Error = A::Error;

    fn init(&mut self) -> Result<(), Self::Error> {
        match self {
            Self::Primary(d) => d.init(),
            Self::Fallback(d) => d.init(),
        }
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        match self {
            Self::Primary(d) => d.poll_card_presence(),
            Self::Fallback(d) => d.poll_card_presence(),
        }
    }

    fn is_card_present(&mut self) -> bool {
        match self {
            Self::Primary(d) => d.is_card_present(),
            Self::Fallback(d) => d.is_card_present(),
        }
    }

    fn session_active(&self) -> bool {
        match self {
            Self::Primary(d) => d.session_active(),
            Self::Fallback(d) => d.session_active(),
        }
    }

    fn presence_poll_detects_removal(&self) -> bool {
        // #369 P2: delegate to the live arm; an unavailable arm must
        // NOT claim the capability.
        match self {
            Self::Primary(d) => d.presence_poll_detects_removal(),
            Self::Fallback(d) => d.presence_poll_detects_removal(),
        }
    }

    fn exchange_decomposition(&self) -> Option<(u32, u32, u32, u32)> {
        match self {
            Self::Primary(d) => d.exchange_decomposition(),
            Self::Fallback(d) => d.exchange_decomposition(),
        }
    }

    fn i2c_timing_probe(&mut self, reads: u32) -> Option<u32> {
        match self {
            Self::Primary(d) => d.i2c_timing_probe(reads),
            Self::Fallback(d) => d.i2c_timing_probe(reads),
        }
    }

    fn configured_fwt_ms(&self) -> u32 {
        match self {
            Self::Primary(d) => d.configured_fwt_ms(),
            Self::Fallback(d) => d.configured_fwt_ms(),
        }
    }

    fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
        match self {
            Self::Primary(d) => d.power_on(atr_buf),
            Self::Fallback(d) => d.power_on(atr_buf),
        }
    }

    fn power_off(&mut self) {
        match self {
            Self::Primary(d) => d.power_off(),
            Self::Fallback(d) => d.power_off(),
        }
    }

    fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Self::Error> {
        match self {
            Self::Primary(d) => d.transmit_apdu(command, response),
            Self::Fallback(d) => d.transmit_apdu(command, response),
        }
    }

    fn card_uid(&self) -> Option<&[u8]> {
        match self {
            Self::Primary(d) => d.card_uid(),
            Self::Fallback(d) => d.card_uid(),
        }
    }

    fn reinit_count(&self) -> u32 {
        match self {
            Self::Primary(d) => d.reinit_count(),
            Self::Fallback(d) => d.reinit_count(),
        }
    }

    fn wtx_counters(&self) -> Option<(u32, u32)> {
        match self {
            Self::Primary(d) => d.wtx_counters(),
            Self::Fallback(d) => d.wtx_counters(),
        }
    }
}

/// Diagnostics ride the live arm (t_4864e74e): the fallback arm IS the
/// unavailable classification, so plain delegation suffices.
impl<A, B> FrontendDiagnostics for EitherFrontend<A, B>
where
    A: NfcDriver + FrontendDiagnostics,
    B: NfcDriver<Error = A::Error> + FrontendDiagnostics,
{
    fn frontend_status(&self) -> FrontendStatus {
        match self {
            Self::Primary(d) => d.frontend_status(),
            Self::Fallback(d) => d.frontend_status(),
        }
    }

    fn frontend_self_test(&mut self) -> Result<(), FrontendErrorKind> {
        match self {
            Self::Primary(d) => d.frontend_self_test(),
            Self::Fallback(d) => d.frontend_self_test(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test frontend: `init()` fails until `fail_inits_remaining` hits
    /// zero; probes report fixed values so gating is observable.
    struct FlakyDriver {
        fail_inits_remaining: usize,
        present: bool,
        session: bool,
    }

    impl FlakyDriver {
        fn failing(fail_inits_remaining: usize) -> Self {
            Self {
                fail_inits_remaining,
                present: true,
                session: false,
            }
        }
    }

    impl FrontendDiagnostics for FlakyDriver {
        fn frontend_status(&self) -> FrontendStatus {
            // t_4864e74e: the inner view — the wrapper test below
            // proves the WRAPPER classification wins when degraded.
            FrontendStatus {
                health: FrontendHealth::Healthy,
                reinit_count: 7,
                ..FrontendStatus::ok()
            }
        }

        fn frontend_self_test(&mut self) -> Result<(), FrontendErrorKind> {
            Ok(())
        }
    }

    impl NfcDriver for FlakyDriver {
        type Error = NfcError;

        fn init(&mut self) -> Result<(), Self::Error> {
            if self.fail_inits_remaining > 0 {
                self.fail_inits_remaining -= 1;
                return Err(NfcError::CommunicationError);
            }
            Ok(())
        }

        fn is_card_present(&mut self) -> bool {
            self.present
        }

        fn poll_card_presence(&mut self) -> PresenceState {
            PresenceState {
                present: self.present,
            }
        }

        fn session_active(&self) -> bool {
            self.session
        }

        fn power_on(&mut self, _atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
            self.session = true;
            Ok(2)
        }

        fn power_off(&mut self) {
            self.session = false;
        }

        fn transmit_apdu(
            &mut self,
            _command: &[u8],
            response: &mut [u8],
        ) -> Result<usize, Self::Error> {
            if !self.session {
                return Err(NfcError::NotInitialized);
            }
            response[..2].copy_from_slice(&[0x90, 0x00]);
            Ok(2)
        }

        fn exchange_decomposition(&self) -> Option<(u32, u32, u32, u32)> {
            Some((1, 2, 3, 4))
        }

        fn i2c_timing_probe(&mut self, _reads: u32) -> Option<u32> {
            Some(99)
        }

        fn configured_fwt_ms(&self) -> u32 {
            42
        }

        fn reinit_count(&self) -> u32 {
            7
        }

        fn wtx_counters(&self) -> Option<(u32, u32)> {
            Some((5, 6))
        }
    }

    #[test]
    fn degraded_frontend_serves_absent_until_init_recovers() {
        let mut frontend = RetryFrontend::degraded(FlakyDriver::failing(2));

        // Polls 1-2: init retried and still failing → absent, degraded.
        assert!(!frontend.poll_card_presence().present);
        assert!(frontend.is_degraded());
        assert!(!frontend.poll_card_presence().present);
        assert!(frontend.is_degraded());

        // Poll 3: init succeeds → healthy → the card (on the coil the
        // whole time) is visible again.
        assert!(frontend.poll_card_presence().present);
        assert!(!frontend.is_degraded());
    }

    #[test]
    fn healthy_frontend_delegates_presence_and_session() {
        let mut inner = FlakyDriver::failing(0);
        inner.session = true;
        let mut frontend = RetryFrontend::healthy(inner);

        assert!(frontend.poll_card_presence().present);
        assert!(frontend.session_active());
        assert_eq!(frontend.exchange_decomposition(), Some((1, 2, 3, 4)));
        assert_eq!(frontend.i2c_timing_probe(4), Some(99));
        assert_eq!(frontend.configured_fwt_ms(), 42);
        assert_eq!(frontend.reinit_count(), 7);
        assert_eq!(frontend.wtx_counters(), Some((5, 6)));
    }

    #[test]
    fn init_failure_flips_healthy_to_degraded() {
        let mut frontend = RetryFrontend::healthy(FlakyDriver::failing(1));
        assert!(frontend.init().is_err());
        assert!(frontend.is_degraded());
        // ...and the next presence poll retries out of it.
        assert!(frontend.poll_card_presence().present);
        assert!(!frontend.is_degraded());
    }

    #[test]
    fn degraded_frontend_gates_bus_probes_and_uid() {
        let mut frontend = RetryFrontend::degraded(FlakyDriver::failing(1));
        assert_eq!(frontend.exchange_decomposition(), None);
        assert_eq!(frontend.i2c_timing_probe(4), None);
        assert_eq!(frontend.configured_fwt_ms(), 0);
        assert_eq!(frontend.wtx_counters(), None);
        assert_eq!(frontend.card_uid(), None);
        assert!(!frontend.session_active());
        // Recovery restores the probes: the first poll consumes the one
        // queued init failure, the second retries and recovers.
        assert!(!frontend.poll_card_presence().present);
        assert!(frontend.poll_card_presence().present);
        assert_eq!(frontend.exchange_decomposition(), Some((1, 2, 3, 4)));
        assert_eq!(frontend.configured_fwt_ms(), 42);
    }

    /// The MFRC522 shape (Codex P1 on #222): plain `init()` keeps
    /// failing but the recovery-aware initializer succeeds — degraded
    /// retries MUST route through `init_recovering()` or the frontend
    /// stays card-absent forever despite the retries.
    #[test]
    fn degraded_retries_route_through_recovery_initializer() {
        struct NeedsFullReinitDriver;

        impl NfcDriver for NeedsFullReinitDriver {
            type Error = NfcError;

            fn init(&mut self) -> Result<(), Self::Error> {
                Err(NfcError::CommunicationError)
            }

            fn init_recovering(&mut self) -> Result<(), Self::Error> {
                // The threshold-triggered full re-init fixes the chip.
                Ok(())
            }

            fn is_card_present(&mut self) -> bool {
                true
            }

            fn poll_card_presence(&mut self) -> PresenceState {
                PresenceState { present: true }
            }

            fn power_on(&mut self, _atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
                Ok(2)
            }

            fn power_off(&mut self) {}

            fn transmit_apdu(
                &mut self,
                _command: &[u8],
                _response: &mut [u8],
            ) -> Result<usize, Self::Error> {
                Ok(2)
            }
        }

        let mut frontend = RetryFrontend::degraded(NeedsFullReinitDriver);

        // First presence poll: the recovery initializer runs and the
        // card appears — a plain-init retry loop would stay absent.
        assert!(frontend.poll_card_presence().present);
        assert!(!frontend.is_degraded());
    }

    #[test]
    fn wrapper_classifies_health_over_the_inner_snapshot() {
        // t_4864e74e: the wrapper owns the degraded bit, so a degraded
        // wrapper reports Degraded even while the inner snapshot says
        // Healthy; a healthy wrapper passes the inner view through.
        let mut degraded = RetryFrontend::degraded(FlakyDriver::failing(1));
        assert_eq!(degraded.frontend_status().health, FrontendHealth::Degraded);
        // Recovery flips the wrapper's classification to Healthy.
        let _ = degraded.poll_card_presence(); // consumes one failure
        let _ = degraded.poll_card_presence(); // recovers
        assert_eq!(degraded.frontend_status().health, FrontendHealth::Healthy);
        assert_eq!(degraded.frontend_status().reinit_count, 7);

        let healthy = RetryFrontend::healthy(FlakyDriver::failing(0));
        assert_eq!(healthy.frontend_status().health, FrontendHealth::Healthy);
    }

    #[test]
    fn unavailable_and_either_surface_unavailable_health() {
        use crate::nfc::MockNfcDriver;
        let mut fallback = EitherFrontend::<
            RetryFrontend<MockNfcDriver>,
            RetryFrontend<UnavailableNfcDriver>,
        >::Fallback(RetryFrontend::degraded(UnavailableNfcDriver));
        assert_eq!(
            fallback.frontend_status().health,
            FrontendHealth::Unavailable
        );
        assert_eq!(
            fallback.frontend_self_test(),
            Err(FrontendErrorKind::NotInitialized)
        );
    }

    #[test]
    fn unavailable_driver_never_recovers_and_fails_cleanly() {
        let mut frontend = RetryFrontend::degraded(UnavailableNfcDriver);

        for _ in 0..3 {
            assert!(!frontend.poll_card_presence().present);
            assert!(frontend.is_degraded());
        }
        let mut atr = [0u8; 33];
        assert_eq!(frontend.power_on(&mut atr), Err(NfcError::NotInitialized));
        let mut resp = [0u8; 64];
        assert_eq!(
            frontend.transmit_apdu(&[0x00], &mut resp),
            Err(NfcError::NotInitialized)
        );
    }
}
