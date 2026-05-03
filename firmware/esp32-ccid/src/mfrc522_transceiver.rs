use iso14443::type_a::{vec::FrameVec, Frame, PcdTransceiver};

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
use embedded_hal::i2c::I2c;
#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
use mfrc522::comm::blocking::i2c::I2cInterface;
#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
use mfrc522::{Initialized, Mfrc522, Register};

#[cfg(not(target_arch = "xtensa"))]
pub struct Mfrc522Transceiver;

#[cfg(not(target_arch = "xtensa"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mfrc522StubError {
    NotAvailable,
}

#[cfg(not(target_arch = "xtensa"))]
impl PcdTransceiver for Mfrc522Transceiver {
    type Error = Mfrc522StubError;

    fn transceive(&mut self, _: &Frame) -> Result<FrameVec, Self::Error> {
        Err(Mfrc522StubError::NotAvailable)
    }

    fn try_enable_hw_crc(&mut self) -> Result<(), Self::Error> {
        Err(Mfrc522StubError::NotAvailable)
    }
}

// ---------------------------------------------------------------------------
// ESP32 (xtensa) real implementation
// ---------------------------------------------------------------------------

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
pub struct Mfrc522Transceiver<I2C: I2c> {
    pub mfrc522: Mfrc522<I2cInterface<I2C>, Initialized>,
}

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
#[derive(Debug)]
pub enum Mfrc522TransceiverError<E> {
    Comm(E),
    Timeout,
    Crc,
    Protocol,
    BufferOverflow,
}

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
impl<E> From<mfrc522::Error<E>> for Mfrc522TransceiverError<E> {
    fn from(error: mfrc522::Error<E>) -> Self {
        match error {
            mfrc522::Error::Comm(err) => Self::Comm(err),
            mfrc522::Error::Timeout => Self::Timeout,
            mfrc522::Error::Crc => Self::Crc,
            mfrc522::Error::BufferOverflow | mfrc522::Error::NoRoom => Self::BufferOverflow,
            mfrc522::Error::Bcc
            | mfrc522::Error::Collision
            | mfrc522::Error::IncompleteFrame
            | mfrc522::Error::Overheating
            | mfrc522::Error::Parity
            | mfrc522::Error::Protocol
            | mfrc522::Error::Wr
            | mfrc522::Error::Nak
            | mfrc522::Error::Proprietary => Self::Protocol,
        }
    }
}

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
impl<I2C: I2c> Mfrc522Transceiver<I2C> {
    pub fn new(mfrc522: Mfrc522<I2cInterface<I2C>, Initialized>) -> Self {
        Self { mfrc522 }
    }

    pub fn reset_comm_regs(&mut self) -> Result<(), mfrc522::Error<I2C::Error>> {
        self.mfrc522.write_register(Register::TxModeReg, 0x00)?;
        self.mfrc522.write_register(Register::RxModeReg, 0x00)?;
        self.mfrc522.write_register(Register::ModWidthReg, 0x26)?;
        Ok(())
    }

    pub fn set_timeout_ms(&mut self, ms: u32) -> Result<(), mfrc522::Error<I2C::Error>> {
        let prescaler = 0xA9u16;
        let timer_freq_hz = 13_560_000u32 / (2 * prescaler as u32 + 1);
        let reload = (ms * timer_freq_hz / 1000).min(0xFFFF);
        self.mfrc522.write_register(Register::TModeReg, 0x80)?;
        self.mfrc522
            .write_register(Register::TPrescalerReg, prescaler as u8)?;
        self.mfrc522
            .write_register(Register::TReloadRegHigh, (reload >> 8) as u8)?;
        self.mfrc522
            .write_register(Register::TReloadRegLow, reload as u8)?;
        log::trace!("set_timeout_ms({}): reload={}", ms, reload);
        Ok(())
    }

    pub fn enable_hw_crc(&mut self) -> Result<(), mfrc522::Error<I2C::Error>> {
        self.mfrc522.write_register(Register::TxModeReg, 0x80)?;
        self.mfrc522.write_register(Register::RxModeReg, 0x80)?;
        log::info!("HW CRC enabled: TxMode=0x80 RxMode=0x80");
        Ok(())
    }
}

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
impl<I2C> PcdTransceiver for Mfrc522Transceiver<I2C>
where
    I2C: I2c,
{
    type Error = Mfrc522TransceiverError<I2C::Error>;

    fn transceive(&mut self, frame: &Frame) -> Result<FrameVec, Self::Error> {
        match frame {
            Frame::Short(data) => {
                log::trace!(
                    "TX Short({}bits): {:02X?}",
                    data.len() * 8 - 8 + 7,
                    data.as_slice()
                );
                let fifo = self.mfrc522.transceive::<2>(data.as_slice(), 7, 0)?;
                if fifo.valid_bits != 0 {
                    log::error!("RX Short: valid_bits={} (expected 0)", fifo.valid_bits);
                    return Err(Mfrc522TransceiverError::Protocol);
                }
                let result = fifo.buffer[..fifo.valid_bytes].to_vec();
                log::trace!("RX Short({}): {:02X?}", fifo.valid_bytes, &result);
                Ok(result)
            }
            Frame::BitOriented(data) => {
                self.mfrc522
                    .rmw_register(Register::CollReg, |b| b & !0x80)?;
                let tx_last_bits = data.get(1).copied().unwrap_or_default() & 0x07;
                log::trace!(
                    "TX Bit({}B, last_bits={}): {:02X?}",
                    data.len(),
                    tx_last_bits,
                    data.as_slice()
                );
                let fifo =
                    self.mfrc522
                        .transceive::<5>(data.as_slice(), tx_last_bits, tx_last_bits)?;
                if fifo.valid_bits != 0 {
                    log::error!(
                        "RX BitOriented: valid_bits={} (expected 0)",
                        fifo.valid_bits
                    );
                    return Err(Mfrc522TransceiverError::Protocol);
                }
                let result = fifo.buffer[..fifo.valid_bytes].to_vec();
                log::trace!("RX BitOriented({}): {:02X?}", fifo.valid_bytes, &result);
                Ok(result)
            }
            Frame::Standard(data) => {
                self.mfrc522
                    .rmw_register(Register::CollReg, |b| b & !0x80)?;

                log::info!("TX Standard({}): {:02X?}", data.len(), data);
                let fifo = match self.mfrc522.transceive::<64>(data, 0, 0) {
                    Ok(f) => f,
                    Err(mfrc522::Error::Crc) => {
                        let irq = self
                            .mfrc522
                            .read_register(Register::ComIrqReg)
                            .unwrap_or(0xFF);
                        let err = self
                            .mfrc522
                            .read_register(Register::ErrorReg)
                            .unwrap_or(0xFF);
                        let level = self
                            .mfrc522
                            .read_register(Register::FIFOLevelReg)
                            .unwrap_or(0) as usize;
                        let mut buf = [0u8; 64];
                        for i in 0..level.min(64) {
                            buf[i] = self
                                .mfrc522
                                .read_register(Register::FIFODataReg)
                                .unwrap_or(0xFF);
                        }
                        log::warn!(
                            "RX Standard CRC ERR: FIFO={} bytes {:02X?} [ComIrq=0x{:02X} Err=0x{:02X}]",
                            level,
                            &buf[..level.min(64)],
                            irq,
                            err
                        );
                        return Err(Mfrc522TransceiverError::Crc);
                    }
                    Err(e) => return Err(e.into()),
                };
                if fifo.valid_bits != 0 {
                    log::error!("RX Standard: valid_bits={} (expected 0)", fifo.valid_bits);
                    return Err(Mfrc522TransceiverError::Protocol);
                }
                let irq = self
                    .mfrc522
                    .read_register(Register::ComIrqReg)
                    .unwrap_or(0xFF);
                let err = self
                    .mfrc522
                    .read_register(Register::ErrorReg)
                    .unwrap_or(0xFF);
                log::info!(
                    "RX Standard({}): {:02X?} [ComIrq=0x{:02X} Err=0x{:02X}]",
                    fifo.valid_bytes,
                    &fifo.buffer[..fifo.valid_bytes],
                    irq,
                    err
                );
                let result = fifo.buffer[..fifo.valid_bytes].to_vec();
                Ok(result)
            }
        }
    }

    fn try_enable_hw_crc(&mut self) -> Result<(), Self::Error> {
        Err(Mfrc522TransceiverError::Protocol)
    }

    fn try_set_timeout_ms(&mut self, ms: u32) -> Result<(), ()> {
        self.set_timeout_ms(ms).map_err(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_arch = "xtensa"))]
    fn test_stub_returns_error() {
        let mut t = Mfrc522Transceiver;

        assert!(t.try_enable_hw_crc().is_err());
        assert!(t.transceive(&Frame::Standard(vec![0x00])).is_err());
    }
}
