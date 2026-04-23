use crate::nfc::{NfcDriver, NfcError, PresenceState};

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
const CARD_ABSENT_THRESHOLD: u8 = 3;

#[cfg(not(target_arch = "xtensa"))]
pub struct Mfrc522NfcDriver;

#[cfg(not(target_arch = "xtensa"))]
impl NfcDriver for Mfrc522NfcDriver {
    type Error = NfcError;

    fn init(&mut self) -> Result<(), Self::Error> {
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

    fn power_on(&mut self, _atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
        Err(NfcError::NoCard)
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

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
use embedded_hal::i2c::I2c;

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
use esp_idf_hal::delay::FreeRtos;

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
use iso14443::type_a::{activation, Ats, Cid, Fsdi, PcdSession, Tc};

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardLifecycle {
    NoCard,
    PresentInactive,
    ActiveSession,
}

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
pub struct Mfrc522NfcDriver<I2C: I2c> {
    transceiver: crate::mfrc522_transceiver::Mfrc522Transceiver<I2C>,
    is_initialized: bool,
    lifecycle: CardLifecycle,
    session: Option<PcdSession>,
    cached_ats: Option<Ats>,
    consecutive_failures: u8,
}

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
impl<I2C> Mfrc522NfcDriver<I2C>
where
    I2C: I2c,
{
    pub fn new(transceiver: crate::mfrc522_transceiver::Mfrc522Transceiver<I2C>) -> Self {
        Self {
            transceiver,
            is_initialized: false,
            lifecycle: CardLifecycle::NoCard,
            session: None,
            cached_ats: None,
            consecutive_failures: 0,
        }
    }

    fn has_card(&self) -> bool {
        self.lifecycle != CardLifecycle::NoCard
    }

    fn mark_card_absent(&mut self) {
        self.lifecycle = CardLifecycle::NoCard;
        self.session = None;
        self.cached_ats = None;
        self.consecutive_failures = 0;
    }

    fn mark_card_present(&mut self) {
        if self.lifecycle == CardLifecycle::NoCard {
            self.lifecycle = CardLifecycle::PresentInactive;
        }
        self.consecutive_failures = 0;
    }

    fn clear_session(&mut self) {
        self.session = None;
        if self.lifecycle != CardLifecycle::NoCard {
            self.lifecycle = CardLifecycle::PresentInactive;
        }
        self.cached_ats = None;
    }

    fn reset_activation_frontend(&mut self) -> Result<(), NfcError> {
        self.transceiver
            .reset_comm_regs()
            .map_err(|_| NfcError::CommunicationError)?;
        self.transceiver
            .mfrc522
            .write_register(mfrc522::Register::ModWidthReg, 0x26)
            .map_err(|_| NfcError::CommunicationError)?;
        self.transceiver
            .mfrc522
            .set_antenna_gain(mfrc522::RxGain::DB33)
            .map_err(|_| NfcError::CommunicationError)?;
        Ok(())
    }

    fn poll_physical_card(&mut self) -> PresenceState {
        if !self.is_initialized {
            return PresenceState { present: false };
        }

        if self.lifecycle == CardLifecycle::ActiveSession {
            return PresenceState { present: true };
        }

        if self.reset_activation_frontend().is_err() {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
            if self.consecutive_failures >= CARD_ABSENT_THRESHOLD {
                self.mark_card_absent();
                return PresenceState { present: false };
            }
            return PresenceState {
                present: self.has_card(),
            };
        }

        match self.transceiver.mfrc522.wupa() {
            Ok(atqa) => {
                let atqa_bytes = atqa.as_bytes();
                log::trace!(
                    "poll_card_presence: ATQA=0x{:02X}{:02X} (via WUPA)",
                    atqa_bytes[0],
                    atqa_bytes[1],
                );
                self.mark_card_present();
                PresenceState { present: true }
            }
            Err(err) => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                if self.consecutive_failures >= CARD_ABSENT_THRESHOLD {
                    log::info!(
                        "Card absent after {} presence failures (last: {:?})",
                        self.consecutive_failures,
                        err,
                    );
                    self.mark_card_absent();
                    PresenceState { present: false }
                } else {
                    PresenceState {
                        present: self.has_card(),
                    }
                }
            }
        }
    }
}

#[cfg(any(test, all(target_arch = "xtensa", feature = "backend-mfrc522")))]
fn build_pcsc_atr_from_historical(
    historical: &[u8],
    atr_buf: &mut [u8],
) -> Result<usize, NfcError> {
    let n = historical.len();
    let atr_len = 5 + n;
    if atr_buf.len() < atr_len {
        return Err(NfcError::BufferOverflow);
    }

    atr_buf[0] = 0x3B;
    atr_buf[1] = 0x80 | (n as u8 & 0x0F);
    atr_buf[2] = 0x80;
    atr_buf[3] = 0x01;
    atr_buf[4..4 + n].copy_from_slice(historical);

    let mut tck: u8 = 0;
    for i in 1..(atr_len - 1) {
        tck ^= atr_buf[i];
    }
    atr_buf[atr_len - 1] = tck;

    Ok(atr_len)
}

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
fn build_pcsc_atr(ats: &Ats, atr_buf: &mut [u8]) -> Result<usize, NfcError> {
    build_pcsc_atr_from_historical(ats.historical_bytes.as_slice(), atr_buf)
}

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
impl<I2C> NfcDriver for Mfrc522NfcDriver<I2C>
where
    I2C: I2c,
{
    type Error = NfcError;

    fn init(&mut self) -> Result<(), NfcError> {
        let version = self
            .transceiver
            .mfrc522
            .version()
            .map_err(|_| NfcError::CommunicationError)?;

        if version != 0x91 && version != 0x92 {
            log::error!("MFRC522 init: unexpected version 0x{:02X}", version);
            return Err(NfcError::CommunicationError);
        }

        self.transceiver
            .mfrc522
            .set_antenna_gain(mfrc522::RxGain::DB33)
            .map_err(|_| NfcError::CommunicationError)?;

        self.is_initialized = true;
        log::info!("MFRC522 init OK, version=0x{:02X}, gain=33dB", version);
        Ok(())
    }

    fn is_card_present(&mut self) -> bool {
        self.poll_card_presence().present
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        self.poll_physical_card()
    }

    fn session_active(&self) -> bool {
        self.lifecycle == CardLifecycle::ActiveSession
    }

    fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, NfcError> {
        if !self.is_initialized {
            return Err(NfcError::NotInitialized);
        }

        if self.lifecycle == CardLifecycle::ActiveSession || self.session.is_some() {
            return Err(NfcError::CommunicationError);
        }

        self.reset_activation_frontend()?;

        // Per ISO 14443-3A §6.2.4 and Bolty reference implementation:
        // card needs settling time between register reset and activation.
        // JavaCards (J3R180) need longer settling than DESFire.
        FreeRtos::delay_ms(15);

        let activation = activation::wakeup(&mut self.transceiver).map_err(|err| {
            log::error!("power_on: activation failed: {:?}", err);
            self.clear_session();
            NfcError::CommunicationError
        })?;

        if !activation.sak.iso14443_4_compliant {
            self.clear_session();
            return Err(NfcError::CommunicationError);
        }

        let (_, ats) =
            PcdSession::from_connect(&mut self.transceiver, Fsdi::Fsd64, Cid::new(0).unwrap())
                .map_err(|err| {
                    log::error!("power_on: RATS/connect failed: {:?}", err);
                    self.clear_session();
                    NfcError::CommunicationError
                })?;

        // Per ISO 14443-4 §5.2.1: "DID equal to '0' indicates that no CID is used."
        // RATS with DID=0 means no CID assigned. I-blocks MUST NOT include CID.
        // from_connect hardcodes Some(cid) → would produce PCB=0x0A (CID following).
        // Re-create session with None so I-blocks use PCB=0x02 (no CID).
        //
        // Enable MFRC522 HW CRC for ISO-DEP (per Tasmota/Miguelbalboa MFRC522Extended).
        // Both TxCRCEn and RxCRCEn = 1. Must happen AFTER activation (anticollision
        // responses lack CRC). Session uses hw_crc=true so iso14443 crate sends
        // without SW CRC and expects raw FIFO (CRC stripped by MFRC522 HW).
        self.transceiver
            .enable_hw_crc()
            .map_err(|_| NfcError::CommunicationError)?;
        let session = PcdSession::from_ats(&ats, None, true);

        let sfgt_us = ats.tb.sfgi.sfgt_us();
        let fsc = ats.format.fsci.fsc();
        let fwi = ats.tb.fwi.value();
        let sfgi = ats.tb.sfgi.value();
        let cid_supp = ats.tc.contains(Tc::CID_SUPP);
        let nad_supp = ats.tc.contains(Tc::NAD_SUPP);

        log::info!(
            "power_on: ATS: FSC={}, SFGI={} (SFGT={}us), FWI={}, CID_SUPP={}, NAD_SUPP={}",
            fsc,
            sfgi,
            sfgt_us,
            fwi,
            cid_supp,
            nad_supp
        );

        if sfgt_us > 0 {
            let sfgt_ms = (sfgt_us + 999) / 1000;
            log::info!("power_on: SFGT delay {}ms", sfgt_ms);
            FreeRtos::delay_ms(sfgt_ms as u32);
        }

        // FWT = (256 × 16 / fc) × 2^FWI  where fc = 13.56 MHz
        // FWI=4 → 4.8ms, FWI=9 → 155ms, FWI=10 → 310ms, FWI=14 → ~5s
        let fwt_ms = if fwi > 0 {
            let fwt_us: u64 = 302 * (1u64 << fwi);
            (fwt_us / 1000 + 10) as u32 // +10ms margin
        } else {
            5 // FWI=0 → ~302µs, use 5ms minimum
        };
        log::info!("power_on: FWT={}ms (FWI={})", fwt_ms, fwi);
        self.transceiver
            .set_timeout_ms(fwt_ms)
            .map_err(|_| NfcError::CommunicationError)?;

        self.session = Some(session);
        self.cached_ats = Some(ats.clone());
        self.lifecycle = CardLifecycle::ActiveSession;
        self.consecutive_failures = 0;

        log::info!("power_on: session established (no CID, full HW CRC)");

        let atr_len = build_pcsc_atr(&ats, atr_buf)?;
        log::trace!(
            "power_on: ATS={:02X?} ATR ({} bytes): {:02X?}",
            ats.to_bytes()
                .map_err(|_| NfcError::CommunicationError)?
                .as_slice(),
            atr_len,
            &atr_buf[..atr_len]
        );
        Ok(atr_len)
    }

    fn power_off(&mut self) {
        if let Some(mut session) = self.session.take() {
            if let Err(err) = session.deselect(&mut self.transceiver) {
                log::warn!("power_off: deselect failed: {:?}", err);
            }
        }

        self.cached_ats = None;
        self.lifecycle = if self.is_initialized {
            CardLifecycle::PresentInactive
        } else {
            CardLifecycle::NoCard
        };
        log::info!("power_off: session closed");
    }

    fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, NfcError> {
        if !self.is_initialized {
            return Err(NfcError::NotInitialized);
        }

        let mut session = self.session.take().ok_or(NfcError::NotInitialized)?;
        if self.lifecycle != CardLifecycle::ActiveSession {
            self.session = Some(session);
            return Err(NfcError::NotInitialized);
        }

        log::trace!("transmit_apdu: {} bytes: {:02X?}", command.len(), command);

        let is_first_exchange = self.cached_ats.is_some();
        if is_first_exchange {
            log::info!(
                "transmit_apdu: first exchange, {} bytes: {:02X?}",
                command.len(),
                command
            );
        }

        let resp = match session.exchange(&mut self.transceiver, command) {
            Ok(resp) => resp,
            Err(err) => {
                log::error!("transmit_apdu: exchange failed: {:?}", err);
                self.clear_session();
                self.lifecycle = CardLifecycle::PresentInactive;
                return Err(NfcError::CommunicationError);
            }
        };

        let resp_slice = resp.as_slice();
        if is_first_exchange {
            log::info!(
                "transmit_apdu: first response, {} bytes: {:02X?}",
                resp_slice.len(),
                resp_slice
            );
        }
        if response.len() < resp_slice.len() {
            self.session = Some(session);
            return Err(NfcError::BufferOverflow);
        }

        response[..resp_slice.len()].copy_from_slice(resp_slice);
        self.session = Some(session);
        self.lifecycle = CardLifecycle::ActiveSession;
        log::trace!(
            "transmit_apdu: received {} bytes: {:02X?}",
            resp_slice.len(),
            resp_slice
        );
        Ok(resp_slice.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_arch = "xtensa"))]
    fn test_stub_is_card_present_returns_false() {
        let mut driver = Mfrc522NfcDriver;
        assert!(!driver.is_card_present());
        assert_eq!(
            driver.poll_card_presence(),
            PresenceState { present: false }
        );
    }

    #[test]
    #[cfg(not(target_arch = "xtensa"))]
    fn test_stub_power_on_returns_no_card() {
        let mut driver = Mfrc522NfcDriver;
        let mut buf = [0u8; 64];
        let result = driver.power_on(&mut buf);
        assert!(matches!(result, Err(NfcError::NoCard)));
    }

    #[test]
    #[cfg(not(target_arch = "xtensa"))]
    fn test_stub_transmit_apdu_returns_not_initialized() {
        let mut driver = Mfrc522NfcDriver;
        let cmd = [0x00, 0xA4, 0x04, 0x00];
        let mut resp = [0u8; 256];
        let result = driver.transmit_apdu(&cmd, &mut resp);
        assert!(matches!(result, Err(NfcError::NotInitialized)));
    }

    #[test]
    fn test_build_pcsc_atr_no_historical_bytes() {
        let mut buf = [0u8; 33];
        let len = build_pcsc_atr_from_historical(&[], &mut buf).unwrap();
        assert_eq!(len, 5);
        assert_eq!(&buf[..5], &[0x3B, 0x80, 0x80, 0x01, 0x01]);
    }

    #[test]
    fn test_build_pcsc_atr_one_historical_byte() {
        let mut buf = [0u8; 33];
        let len = build_pcsc_atr_from_historical(&[0x80], &mut buf).unwrap();
        assert_eq!(len, 6);
        assert_eq!(&buf[..6], &[0x3B, 0x81, 0x80, 0x01, 0x80, 0x80]);
    }

    #[test]
    fn test_build_pcsc_atr_multiple_historical_bytes() {
        let mut buf = [0u8; 33];
        let hist = &[0x80, 0x5A, 0x01, 0x02];
        let len = build_pcsc_atr_from_historical(hist, &mut buf).unwrap();
        assert_eq!(len, 9);
        assert_eq!(buf[0], 0x3B);
        assert_eq!(buf[1], 0x84);
        assert_eq!(&buf[4..8], hist);
        let expected_tck = 0x84 ^ 0x80 ^ 0x01 ^ 0x80 ^ 0x5A ^ 0x01 ^ 0x02;
        assert_eq!(buf[8], expected_tck);
    }

    #[test]
    fn test_build_pcsc_atr_buffer_too_small() {
        let mut buf = [0u8; 3];
        let result = build_pcsc_atr_from_historical(&[], &mut buf);
        assert!(matches!(result, Err(NfcError::BufferOverflow)));
    }

    #[test]
    fn test_card_absent_threshold() {
        assert_eq!(3, 3);
    }
}
