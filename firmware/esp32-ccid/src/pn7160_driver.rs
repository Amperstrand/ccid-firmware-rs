//! PN7160 NFC driver — firmware-side NfcDriver delegation to the
//! host-testable `pn7160_nci::driver::Pn7160Driver`.
//!
//! The protocol logic lives entirely in the `pn7160-nci` crate (ladder,
//! reader session, driver). This module is a thin adapter implementing
//! the firmware's `NfcDriver` trait by delegation, keeping hardware
//! binding (I2C transport + VEN) as the only verdict-dependent piece.

use crate::nfc::{NfcDriver, PresenceState};
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
    pub fn transport_mut(&mut self) -> &mut T {
        self.inner.transport_mut()
    }

    /// Recover the transport after a failed session (init ladder or
    /// mid-session health probe) so probing can continue.
    pub fn into_transport(self) -> T {
        self.inner.into_transport()
    }
}

impl<T: Transport> NfcDriver for Pn7160NfcDriver<T> {
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
