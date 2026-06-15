//! PN532 NFC driver for ESP32 + PN532 over SPI.
//!
//! ## Pin mapping (DevKitC V4)
//!
//! | Function | GPIO | Direction |
//! |----------|------|-----------|
//! | SCK      | 19   | SPI       |
//! | MISO     | 18   | SPI       |
//! | MOSI     | 17   | SPI       |
//! | CS/SS    | 25   | SPI       |
//! | IRQ      | 16   | Input     |
//! | RST      | 26   | Output    |
//!
//! SPI: Mode 0, ≤1 MHz, MSB-first (msb-spi feature handles LSB conversion).

#[cfg(any(not(target_arch = "xtensa"), feature = "backend-pn532"))]
use crate::nfc::{NfcDriver, NfcError, PresenceState};

#[cfg(not(target_arch = "xtensa"))]
pub struct Pn532NfcDriver;

#[cfg(not(target_arch = "xtensa"))]
impl NfcDriver for Pn532NfcDriver {
    type Error = NfcError;

    fn init(&mut self) -> Result<(), NfcError> {
        Ok(())
    }

    fn is_card_present(&mut self) -> bool {
        false
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        PresenceState { present: false }
    }

    fn session_active(&self) -> bool {
        false
    }

    fn power_on(&mut self, _atr_buf: &mut [u8]) -> Result<usize, NfcError> {
        Err(NfcError::NoCard)
    }

    fn power_off(&mut self) {}

    fn transmit_apdu(&mut self, _command: &[u8], _response: &mut [u8]) -> Result<usize, NfcError> {
        Err(NfcError::NotInitialized)
    }
}

#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
use core::convert::Infallible;

#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
use pn532::{spi::SPIInterfaceWithIrq, Pn532};
#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
use pn532_transport::{EspDelayTimer, Pn532Device};

/// PN532 internal buffer: must satisfy N-9 >= max(response_len, request_data_len).
/// 64 → 55 byte payload, enough for standard short APDUs.
#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
const PN532_BUF_SIZE: usize = 64;

/// Synthetic ATR: TS=3B T0=80 TD1=80 TD2=01 TCK=01.
/// Sufficient for pcscd to route APDUs; future: build from ATS via RATS.
#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
const SYNTHETIC_ATR: [u8; 5] = [0x3B, 0x80, 0x80, 0x01, 0x01];

#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
fn map_transport_error(e: pn532_transport::Error) -> NfcError {
    match e {
        pn532_transport::Error::NotInitialized => NfcError::NotInitialized,
        pn532_transport::Error::NoCard => NfcError::NoCard,
        pn532_transport::Error::Communication => NfcError::CommunicationError,
        pn532_transport::Error::BufferOverflow => NfcError::BufferOverflow,
    }
}

#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
pub struct Pn532NfcDriver<SPI, IRQ, RST>
where
    SPI: embedded_hal::spi::SpiDevice,
    IRQ: embedded_hal::digital::InputPin<Error = Infallible>,
    RST: embedded_hal::digital::OutputPin,
{
    device: Pn532Device<Pn532<SPIInterfaceWithIrq<SPI, IRQ>, EspDelayTimer, PN532_BUF_SIZE>, RST>,
    session_active: bool,
}

#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
impl<SPI, IRQ, RST> Pn532NfcDriver<SPI, IRQ, RST>
where
    SPI: embedded_hal::spi::SpiDevice,
    IRQ: embedded_hal::digital::InputPin<Error = Infallible>,
    RST: embedded_hal::digital::OutputPin,
{
    pub fn new(spi: SPI, irq: IRQ, rst: RST) -> Result<Self, NfcError> {
        let interface = SPIInterfaceWithIrq { spi, irq };
        let timer = EspDelayTimer::new();
        let pn532 = Pn532::new(interface, timer);
        let device = Pn532Device::new(pn532, rst);

        Ok(Self {
            device,
            session_active: false,
        })
    }
}

#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
impl<SPI, IRQ, RST> NfcDriver for Pn532NfcDriver<SPI, IRQ, RST>
where
    SPI: embedded_hal::spi::SpiDevice,
    IRQ: embedded_hal::digital::InputPin<Error = Infallible>,
    RST: embedded_hal::digital::OutputPin,
{
    type Error = NfcError;

    /// Init: hardware reset → GetFirmwareVersion → SAMConfiguration(Normal).
    fn init(&mut self) -> Result<(), NfcError> {
        self.device.init().map_err(map_transport_error)
    }

    /// InListPassiveTarget for ISO 14443-A; stores target_num on success.
    fn is_card_present(&mut self) -> bool {
        self.poll_card_presence().present
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        let present = self.device.detect_card();
        if !present {
            self.session_active = false;
        }
        PresenceState { present }
    }

    fn session_active(&self) -> bool {
        self.session_active
    }

    /// Returns synthetic ATR `3B 80 80 01 01` for all detected cards.
    fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, NfcError> {
        if !self.device.is_initialized() {
            return Err(NfcError::NotInitialized);
        }
        // Defensive: try one more detection if no card is known to be present.
        if !self.device.card_present() {
            if !self.device.detect_card() {
                return Err(NfcError::NoCard);
            }
        }
        if atr_buf.len() < SYNTHETIC_ATR.len() {
            return Err(NfcError::BufferOverflow);
        }

        atr_buf[..SYNTHETIC_ATR.len()].copy_from_slice(&SYNTHETIC_ATR);
        self.session_active = true;
        Ok(SYNTHETIC_ATR.len())
    }

    /// InRelease to deselect the current target.
    fn power_off(&mut self) {
        self.device.release_card();
        self.session_active = false;
    }

    /// InDataExchange with selected target.
    fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, NfcError> {
        if !self.session_active {
            return Err(NfcError::NotInitialized);
        }

        let result = self
            .device
            .exchange_apdu(command)
            .map_err(map_transport_error)?;

        if response.len() < result.len() {
            return Err(NfcError::BufferOverflow);
        }
        response[..result.len()].copy_from_slice(&result);
        Ok(result.len())
    }
}
