#![allow(dead_code)]

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresenceState {
    pub present: bool,
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
        }
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
        self.card_present
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        if !self.card_present {
            self.session_active = false;
        }
        PresenceState {
            present: self.card_present,
        }
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

    fn transmit_apdu(
        &mut self,
        _command: &[u8],
        response: &mut [u8],
    ) -> Result<usize, Self::Error> {
        if !self.card_present {
            self.session_active = false;
            return Err(NfcError::NoCard);
        }

        if !self.session_active {
            return Err(NfcError::NotInitialized);
        }

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
