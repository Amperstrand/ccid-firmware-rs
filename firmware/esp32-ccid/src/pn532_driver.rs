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
use core::time::Duration;

#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
use esp_idf_hal::delay::Delay;

#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
use pn532::{
    requests::{BorrowedRequest, Command, SAMMode},
    spi::SPIInterfaceWithIrq,
    CountDown, IntoDuration, Pn532, Request,
};

/// Adapter from esp-idf blocking delay to pn532::CountDown.
/// `start(d)` records duration; `wait()` blocks for it then returns Ok.
#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
pub struct EspDelayTimer {
    deadline: Duration,
}

#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
impl EspDelayTimer {
    pub fn new() -> Self {
        Self {
            deadline: Duration::ZERO,
        }
    }
}

#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
impl CountDown for EspDelayTimer {
    type Time = Duration;

    fn start<T>(&mut self, count: T)
    where
        T: Into<Self::Time>,
    {
        self.deadline = count.into();
    }

    fn wait(&mut self) -> nb::Result<(), Infallible> {
        let ms = self.deadline.as_millis() as u32;
        if ms > 0 {
            Delay::new_default().delay_ms(ms);
            self.deadline = Duration::ZERO;
        }
        Ok(())
    }
}

/// PN532 internal buffer: must satisfy N-9 >= max(response_len, request_data_len).
/// 64 → 55 byte payload, enough for standard short APDUs.
#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
const PN532_BUF_SIZE: usize = 64;

/// Synthetic ATR: TS=3B T0=80 TD1=80 TD2=01 TCK=01.
/// Sufficient for pcscd to route APDUs; future: build from ATS via RATS.
#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
const SYNTHETIC_ATR: [u8; 5] = [0x3B, 0x80, 0x80, 0x01, 0x01];

#[cfg(all(target_arch = "xtensa", feature = "backend-pn532"))]
pub struct Pn532NfcDriver<SPI, IRQ, RST>
where
    SPI: embedded_hal::spi::SpiDevice,
    IRQ: embedded_hal::digital::InputPin<Error = Infallible>,
    RST: embedded_hal::digital::OutputPin,
{
    pn532: Pn532<SPIInterfaceWithIrq<SPI, IRQ>, EspDelayTimer, PN532_BUF_SIZE>,
    rst_pin: RST,
    target_num: Option<u8>,
    is_initialized: bool,
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

        Ok(Self {
            pn532,
            rst_pin: rst,
            target_num: None,
            is_initialized: false,
            session_active: false,
        })
    }

    fn hardware_reset(&mut self) -> Result<(), NfcError> {
        self.rst_pin
            .set_low()
            .map_err(|_| NfcError::CommunicationError)?;
        Delay::new_default().delay_ms(100);
        self.rst_pin
            .set_high()
            .map_err(|_| NfcError::CommunicationError)?;
        Delay::new_default().delay_ms(500);
        Ok(())
    }

    fn get_firmware_version(&mut self) -> Result<(u8, u8), NfcError> {
        let response = self
            .pn532
            .process(&Request::GET_FIRMWARE_VERSION, 4, 50.ms())
            .map_err(|_| NfcError::CommunicationError)?;

        if response.len() < 4 || response[0] != 0x32 {
            return Err(NfcError::CommunicationError);
        }

        Ok((response[1], response[2]))
    }

    fn configure_sam(&mut self) -> Result<(), NfcError> {
        self.pn532
            .process(
                &Request::sam_configuration(SAMMode::Normal, false),
                0,
                50.ms(),
            )
            .map_err(|_| NfcError::CommunicationError)?;
        Ok(())
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
        self.hardware_reset()?;
        self.get_firmware_version()?;
        self.configure_sam()?;
        self.is_initialized = true;
        Ok(())
    }

    /// InListPassiveTarget for ISO 14443-A; stores target_num on success.
    fn is_card_present(&mut self) -> bool {
        self.poll_card_presence().present
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        if !self.is_initialized {
            return PresenceState { present: false };
        }

        let present = match self
            .pn532
            .process(&Request::INLIST_ONE_ISO_A_TARGET, 20, 1000.ms())
        {
            Ok(response) if !response.is_empty() && response[0] > 0 => {
                self.target_num = Some(1);
                true
            }
            _ => {
                self.target_num = None;
                self.session_active = false;
                false
            }
        };

        PresenceState { present }
    }

    fn session_active(&self) -> bool {
        self.session_active
    }

    /// Returns synthetic ATR `3B 80 80 01 01` for all detected cards.
    fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, NfcError> {
        if !self.is_initialized {
            return Err(NfcError::NotInitialized);
        }
        if self.target_num.is_none() && !self.poll_card_presence().present {
            return Err(NfcError::NoCard);
        }
        if self.target_num.is_none() {
            return Err(NfcError::NoCard);
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
        if let Some(target_num) = self.target_num {
            let _ = self
                .pn532
                .process(&Request::new(Command::InRelease, [target_num]), 0, 50.ms());
            self.target_num = None;
        }
        self.session_active = false;
    }

    /// InDataExchange with selected target.
    /// Response[0]=status (0=ok), rest is APDU data+SW.
    fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, NfcError> {
        if !self.is_initialized {
            return Err(NfcError::NotInitialized);
        }
        if !self.session_active {
            return Err(NfcError::NotInitialized);
        }
        let target_num = self.target_num.ok_or(NfcError::NoCard)?;

        let mut data = heapless::Vec::<u8, PN532_BUF_SIZE>::new();
        data.push(target_num)
            .map_err(|_| NfcError::BufferOverflow)?;
        data.extend_from_slice(command)
            .map_err(|_| NfcError::BufferOverflow)?;

        let request = BorrowedRequest::new(Command::InDataExchange, &data);

        let result = self
            .pn532
            .process(request, PN532_BUF_SIZE, 1000.ms())
            .map_err(|_| NfcError::CommunicationError)?;

        if result.is_empty() || result[0] != 0x00 {
            self.session_active = false;
            return Err(NfcError::CommunicationError);
        }

        let apdu_response = &result[1..];
        if response.len() < apdu_response.len() {
            return Err(NfcError::BufferOverflow);
        }
        response[..apdu_response.len()].copy_from_slice(apdu_response);
        Ok(apdu_response.len())
    }
}
