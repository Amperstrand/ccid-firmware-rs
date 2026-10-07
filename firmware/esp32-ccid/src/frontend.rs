//! Retryable NFC frontend wrapper (audit wave 4: frontend fault isolation).
//!
//! The mains historically halted the whole CCID service when the NFC
//! controller failed to initialize — reproducing the commercial-reader
//! failure mode where a wedged frontend takes the reader off the bus. The
//! wrapper keeps the host-facing service alive instead:
//!
//! - card presence polls on a degraded frontend return absent AND retry
//!   `init()` — presence polls are already interval-gated by the serving
//!   loop, so retries come for free at that cadence
//! - power-on / APDU exchange delegate to the inner driver, whose
//!   not-initialized guards fail them cleanly
//! - a successful init (boot or retry) marks the frontend healthy again
//!
//! All host-testable: `MockNfcDriver` drives every path below.

use crate::nfc::{NfcDriver, PresenceState};

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
    /// retry from the first presence poll.
    pub fn degraded(inner: D) -> Self {
        Self { inner, degraded: true }
    }

    pub fn is_degraded(&self) -> bool {
        self.degraded
    }

    pub fn into_inner(self) -> D {
        self.inner
    }
}

impl<D: NfcDriver> NfcDriver for RetryFrontend<D> {
    type Error = D::Error;

    fn init(&mut self) -> Result<(), Self::Error> {
        let result = self.inner.init();
        self.degraded = result.is_err();
        result
    }

    fn is_card_present(&mut self) -> bool {
        self.poll_card_presence().present
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        if self.degraded && self.inner.init().is_ok() {
            self.degraded = false;
        }
        if self.degraded {
            PresenceState { present: false }
        } else {
            self.inner.poll_card_presence()
        }
    }

    fn session_active(&self) -> bool {
        !self.degraded && self.inner.session_active()
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

    fn is_available(&self) -> bool {
        !self.degraded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nfc::{MockNfcDriver, NfcError};

    /// Mock whose init fails until `succeed_after` calls have elapsed.
    struct FlakyInitMock {
        inner: MockNfcDriver,
        failures_left: u32,
        init_calls: u32,
    }

    impl NfcDriver for FlakyInitMock {
        type Error = NfcError;

        fn init(&mut self) -> Result<(), NfcError> {
            self.init_calls += 1;
            if self.failures_left > 0 {
                self.failures_left -= 1;
                return Err(NfcError::CommunicationError);
            }
            self.inner.init()
        }

        fn is_card_present(&mut self) -> bool {
            self.poll_card_presence().present
        }

        fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, NfcError> {
            self.inner.power_on(atr_buf)
        }

        fn power_off(&mut self) {
            self.inner.power_off()
        }

        fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, NfcError> {
            self.inner.transmit_apdu(command, response)
        }
    }

    fn flaky(card_present: bool, failures: u32) -> FlakyInitMock {
        FlakyInitMock {
            inner: MockNfcDriver::new(card_present, &[0x3B, 0x80], &[0x90, 0x00]),
            failures_left: failures,
            init_calls: 0,
        }
    }

    #[test]
    fn degraded_frontend_polls_absent_and_retries_init_each_poll() {
        let mut fe = RetryFrontend::degraded(flaky(true, 2));

        assert!(fe.is_degraded());
        assert!(!fe.is_card_present(), "degraded frontend must report absent");
        assert!(fe.is_degraded(), "init failed on the first poll retry");

        assert!(!fe.is_card_present());
        assert!(fe.is_degraded(), "init failed on the second poll retry too");

        let recovered = fe.poll_card_presence();
        assert!(recovered.present, "third poll's init succeeds and delegates");
        assert!(!fe.is_degraded());
    }

    #[test]
    fn degraded_frontend_recovers_on_successful_retry() {
        let mut fe = RetryFrontend::degraded(flaky(true, 1));
        assert!(!fe.poll_card_presence().present);
        assert!(fe.is_degraded(), "still degraded while init keeps failing");

        // Next poll: init succeeds and presence delegates to the inner mock.
        let presence = fe.poll_card_presence();
        assert!(presence.present, "recovered frontend must delegate presence");
        assert!(!fe.is_degraded());

        let mut atr = [0u8; 33];
        assert_eq!(fe.power_on(&mut atr), Ok(2));
        assert!(fe.session_active());
    }

    #[test]
    fn healthy_wrapper_passes_presence_through() {
        let mut inner = MockNfcDriver::new(true, &[0x3B], &[0x90, 0x00]);
        inner.init().unwrap();
        let mut fe = RetryFrontend::healthy(inner);
        assert!(!fe.is_degraded());
        assert!(fe.is_card_present());
    }

    #[test]
    fn init_failure_marks_healthy_frontend_degraded() {
        let mut fe = RetryFrontend::healthy(flaky(false, 1));
        assert!(fe.init().is_err());
        assert!(fe.is_degraded());
    }
}
