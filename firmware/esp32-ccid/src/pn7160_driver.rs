//! PN7160 NFC driver — firmware-side NfcDriver delegation to the
//! host-testable `pn7160_nci::driver::Pn7160Driver`.
//!
//! The protocol logic lives entirely in the `pn7160-nci` crate (ladder,
//! reader session, driver). This module is a thin adapter implementing
//! the firmware's `NfcDriver` trait by delegation, keeping hardware
//! binding (I2C transport + VEN) as the only verdict-dependent piece.

use crate::nfc::{NfcDriver, PresenceState};
use card_interface::{FrontendDiagnostics, FrontendErrorKind, FrontendHealth, FrontendStatus};
use pn7160_nci::driver::{Error as CoreError, Pn7160Driver};
use pn7160_nci::Transport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NfcError {
    BringUp(&'static str),
    Select(&'static str),
    NoCard,
    ExchangeFailed,
    BufferTooSmall,
}

impl From<CoreError> for NfcError {
    fn from(e: CoreError) -> Self {
        match e {
            CoreError::BringUp(s) => NfcError::BringUp(s),
            CoreError::Select(s) => NfcError::Select(s),
            CoreError::NoCard => NfcError::NoCard,
            CoreError::ExchangeFailed => NfcError::ExchangeFailed,
            CoreError::BufferTooSmall => NfcError::BufferTooSmall,
        }
    }
}

/// Firmware NfcDriver over any pn7160-nci Transport (mock for host tests,
/// I2C + VEN on hardware after the pad-diag verdict resolves issue #62).
pub struct Pn7160NfcDriver<T: Transport> {
    inner: Pn7160Driver<T>,
}

impl<T: Transport> Pn7160NfcDriver<T> {
    pub fn new(transport: T) -> Self {
        Pn7160NfcDriver {
            inner: Pn7160Driver::new(transport),
        }
    }

    /// I2C health probe without giving up the driver (issue #63).
    ///
    /// The link is borrowed raw: only stateless operations (zero-length
    /// probe writes) are safe while a session is active — a raw NCI
    /// transact here would desync the driver's state machine.
    pub fn transport_mut(&mut self) -> &mut T {
        self.inner.transport_mut()
    }

    /// Recover the transport after a failed session (init ladder or
    /// mid-session health probe) so probing can continue.
    pub fn into_transport(self) -> T {
        self.inner.into_transport()
    }

    /// Constructor + bring-up ladder (t_4864e74e): delegates to the
    /// crate's `Pn7160Driver::bring_up` (the ladder moved OUT of the
    /// firmware mains into `pn7160-nci`); this wrapper keeps the
    /// firmware's logging and hands back the NfcDriver-wrapped result.
    /// Takes any constructed transport (pure `Transport` trait — no
    /// board knowledge); `recover` runs between failed attempts (the
    /// esp transport's VEN cycle + probe). Returns the driver and
    /// whether init succeeded — callers wrap with
    /// `RetryFrontend::healthy/degraded`.
    pub fn bring_up<F>(transport: T, attempts: u32, recover: F) -> (Self, bool)
    where
        F: FnMut(&mut T),
    {
        let (inner, ok) = Pn7160Driver::bring_up(transport, attempts, recover);
        if ok {
            log::warn!("bring-up: frontend initialized");
        } else {
            log::warn!("bring-up: frontend unresponsive — card-absent mode");
        }
        (Self { inner }, ok)
    }

    /// Wrap an already-constructed crate driver (the seam's assembly
    /// point for callers that bring the transport up themselves).
    pub fn from_driver(inner: Pn7160Driver<T>) -> Self {
        Self { inner }
    }

    /// Semi-mute streak (ACKed but unanswered re-arm writes) for the
    /// serve loop's VEN-recovery trigger.
    pub fn silent_rearm_streak(&self) -> u8 {
        self.inner.silent_rearm_streak()
    }

    /// Install the exchange stall hook (issue #156 B): the CCID layer's
    /// time-extension emitter, running deep inside a blocking DATA
    /// exchange.
    pub fn set_stall_hook(&mut self, hook: Option<pn7160_nci::reader::StallHook>) {
        self.inner.set_stall_hook(hook);
    }
}

impl<T: Transport> NfcDriver for Pn7160NfcDriver<T> {
    fn poll_maintenance(&mut self, now_ms: u64) {
        self.inner.poll_maintenance(now_ms);
    }
    type Error = NfcError;

    fn init(&mut self) -> Result<(), NfcError> {
        self.inner.init().map_err(NfcError::from)
    }

    fn is_card_present(&mut self) -> bool {
        self.inner.is_card_present()
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        PresenceState {
            present: self.inner.is_card_present(),
        }
    }

    fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, NfcError> {
        self.inner.power_on(atr_buf).map_err(NfcError::from)
    }

    fn power_off(&mut self) {
        self.inner.power_off()
    }

    fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, NfcError> {
        self.inner
            .transmit_apdu(command, response)
            .map_err(NfcError::from)
    }

    fn session_active(&self) -> bool {
        self.inner.session_active()
    }

    fn presence_poll_detects_removal(&self) -> bool {
        // PN7160's `is_card_present` drains the pending
        // RF_DEACTIVATE_NTF, so a card removed mid-session IS observed
        // on the next presence poll (bench-proven #326, 2026-10-21).
        true
    }

    fn wtx_counters(&self) -> Option<(u32, u32)> {
        Some(self.inner.wtx_counters())
    }

    /// Issue #408 diagnosis: surface the cumulative wait at which the last
    /// DATA packet arrived through Escape 0xD2 word 8 (the
    /// max_transceive_us slot on the MFRC522 backend; ms x 1000 here).
    fn exchange_decomposition(&self) -> Option<(u32, u32, u32, u32)> {
        let arrival_ms =
            pn7160_nci::reader::LAST_DATA_ARRIVAL_MS.load(core::sync::atomic::Ordering::Relaxed);
        Some((0, self.inner.wtx_counters().0, 0, arrival_ms * 1000))
    }
}

/// Uniform diagnostics surface (t_4864e74e). The constructed driver is
/// Healthy by definition (an unconstructible frontend never reaches
/// this type — the mains serve the `UnavailableNfcDriver` stand-in);
/// deeper counters stay in the crate until it exposes them.
impl<T: Transport> FrontendDiagnostics for Pn7160NfcDriver<T> {
    fn frontend_status(&self) -> FrontendStatus {
        FrontendStatus {
            health: FrontendHealth::Healthy,
            ..FrontendStatus::ok()
        }
    }

    fn frontend_self_test(&mut self) -> Result<(), FrontendErrorKind> {
        // Stateless link probe only (never an NCI transact — that
        // would desync the driver state machine mid-session).
        if self.inner.transport_mut().link_probe() {
            Ok(())
        } else {
            Err(FrontendErrorKind::Bus)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pn7160_nci::mock::MockTransport;

    #[test]
    fn transport_mut_reaches_the_link_without_giving_up_the_driver() {
        let mut driver = Pn7160NfcDriver::new(MockTransport::new());
        driver.transport_mut().transact(&[0x20, 0x00, 0x00]);
        assert_eq!(driver.transport_mut().sent, vec![vec![0x20, 0x00, 0x00]]);
        // driver still usable afterwards (session_active consults inner state)
        assert!(!driver.session_active());
    }

    #[test]
    fn into_transport_recovers_the_link_with_history_intact() {
        let mut transport = MockTransport::new();
        transport.transact(&[0x2f, 0x00, 0x00]);
        let driver = Pn7160NfcDriver::new(transport);
        let mut recovered = driver.into_transport();
        assert_eq!(recovered.sent, vec![vec![0x2f, 0x00, 0x00]]);
        recovered.transact(&[0x20, 0x00, 0x00]);
        assert_eq!(recovered.sent.len(), 2);
    }
}

#[test]
fn frontend_self_test_rides_the_link_probe() {
    use pn7160_nci::mock::MockTransport;
    let mut driver = Pn7160NfcDriver::new(MockTransport::new());
    assert!(driver.frontend_self_test().is_ok());
    driver.transport_mut().link_alive = false;
    assert_eq!(driver.frontend_self_test(), Err(FrontendErrorKind::Bus));
}

#[test]
fn bring_up_ladder_reports_failure_and_runs_the_recover_hook() {
    use pn7160_nci::mock::MockTransport;
    // Empty mock: no scripted CORE_RESET reply, so init fails on every
    // attempt — the transport must come back out for the recover hook
    // (VEN cycle class), and health reports not-initialized.
    let mock = MockTransport::new();
    let mut recover_calls = 0;
    let (_d, ok) = Pn7160NfcDriver::bring_up(mock, 1, |_| recover_calls += 1);
    assert!(!ok, "unscripted transport cannot initialize");
    assert_eq!(
        recover_calls, 1,
        "recover hook runs once per failed attempt"
    );
}
