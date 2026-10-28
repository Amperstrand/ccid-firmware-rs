//! MFRC522 PcdTransceiver for ISO 14443 Type A communication.
//!
//! Bridges the MFRC522 chip to the `iso14443::type_a::PcdTransceiver` trait.
//! This crate has NO dependency on ntag424 — it is purely the PCD (Proximity
//! Coupling Device) layer, suitable for sharing across projects that need
//! raw ISO 14443 frame exchange.
//!
//! `#![no_std]`-compatible (uses `alloc`).

#![no_std]

pub extern crate alloc;

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;
use iso14443::type_a::vec::FrameVec;
use mfrc522::{Initialized, Mfrc522, Register, RxGain, comm::blocking::i2c::I2cInterface};

pub use iso14443::type_a::{Frame, PcdTransceiver};
pub use mfrc522::Error as Mfrc522Error;

pub const DEFAULT_I2C_ADDRESS: u8 = 0x28;

/// BoardDesc-derived NFC-frontend bus binding (t_4864e74e): the pure
/// numbers a frontend constructor needs, resolved from a
/// `boards::BoardDesc` by the firmware bin (`BusRole::NfcFrontend` +
/// `nfc`) — this crate stays board-blind by design
/// (BOARDS-AS-DATA-DESIGN §3.1: data table × constructor layering).
///
/// `i2c_addr` + `bus_hz` are the controller facts; `bus_hz` is the
/// run speed the BUS impl must be configured for (consumed by the
/// caller when it constructs its embedded-hal bus — e.g.
/// `add_hal_device(addr, hz)` on the shared driver_ng adapter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrontendI2cBinding {
    /// NFC controller I2C address (7-bit).
    pub i2c_addr: u8,
    /// Frontend bus run speed in Hz (probe speed is a bus-level fact).
    pub bus_hz: u32,
}

impl FrontendI2cBinding {
    /// The bench-default binding (MFRC522 @ 0x28, 400 kHz — issue #59
    /// re-attempt with measured RTT table + 100-transaction soak).
    pub const fn new(i2c_addr: u8, bus_hz: u32) -> Self {
        Self { i2c_addr, bus_hz }
    }
}

/// MFRC522 VersionReg values a healthy MFRC522 answers with
/// (datasheet table 19; both observed on the bench).
pub const VERSION_REG_EXPECTED: [u8; 2] = [0x91, 0x92];

/// Field-off dwell for the antenna cycle in
/// [`Mfrc522Transceiver::prepare_for_activation_with_delays`].
///
/// A PICC whose ISO-14443-4 layer wedged mid-chain (e.g. a stalled host
/// transfer) keeps answering WUPA but ignores RATS until the field
/// de-energizes it; 50 ms is what a charge-pump-powered card needs to
/// fully reset (bench-proven on ccid-firmware-rs, commit 89382e6, against
/// a J3R180 JavaCard — the answer is WUPA-alive but RATS-dead without it).
///
/// Only ever paid on the idle poll / activation path — never with a
/// response pending.
pub const FIELD_OFF_DWELL_MS: u32 = 50;

/// Field-on settle after the antenna is re-energized in
/// [`Mfrc522Transceiver::prepare_for_activation_with_delays`] — lets the
/// charge pump stabilize before the first REQA/WUPA.
pub const FIELD_ON_SETTLE_MS: u32 = 5;

/// A no-op [`DelayNs`]: preserves the pre-dwell antenna-cycle timing for
/// callers that cannot provide a real delay (host unit tests, transports
/// without a timer). Prefer injecting a yielding delay on hardware —
/// busy-waiting through field dwells starves the scheduler and, on ESP-IDF,
/// trips the task watchdog (ccid-firmware-rs issues #63/#80 root cause).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoDelay;

impl DelayNs for NoDelay {
    fn delay_ns(&mut self, _ns: u32) {}
}

/// [`DelayNs`] adapter over a plain millisecond function pointer — lets
/// platform-generic callers (the firmware service layer) inject a real
/// delay without threading a second generic parameter through every
/// consumer.
#[derive(Debug, Clone, Copy)]
pub struct FnDelay(pub fn(u32));

impl DelayNs for FnDelay {
    fn delay_ns(&mut self, ns: u32) {
        // Ceiling division: never under-delay.
        (self.0)(ns.div_ceil(1_000_000))
    }
}

/// Higher-level error type providing semantic context over [`Mfrc522Error`].
///
/// [`Mfrc522Error`] remains the [`PcdTransceiver::Error`] associated type for
/// backward compatibility; use [`Error::from`] to convert.
#[derive(Debug, thiserror::Error)]
pub enum Error<E> {
    #[error("MFRC522 communication error: {0:?}")]
    Communication(E),
    #[error("MFRC522 timeout")]
    Timeout,
    #[error("MFRC522 protocol error")]
    Protocol,
    #[error("MFRC522 buffer overflow")]
    BufferOverflow,
}

impl<E> From<Mfrc522Error<E>> for Error<E> {
    fn from(e: Mfrc522Error<E>) -> Self {
        match e {
            Mfrc522Error::Comm(inner) => Error::Communication(inner),
            Mfrc522Error::Timeout => Error::Timeout,
            Mfrc522Error::Protocol => Error::Protocol,
            Mfrc522Error::BufferOverflow | Mfrc522Error::NoRoom => Error::BufferOverflow,
            // All remaining variants (Bcc, Collision, Crc, IncompleteFrame,
            // Overheating, Parity, Wr, Nak, Proprietary) are NFC protocol
            // level issues.
            _ => Error::Protocol,
        }
    }
}

pub struct Mfrc522Transceiver<I2C: I2c> {
    mfrc522: Mfrc522<I2cInterface<I2C>, Initialized>,
}

impl<I2C: I2c> Mfrc522Transceiver<I2C> {
    pub fn new(mfrc522: Mfrc522<I2cInterface<I2C>, Initialized>) -> Self {
        Self { mfrc522 }
    }

    pub fn raw(&mut self) -> &mut Mfrc522<I2cInterface<I2C>, Initialized> {
        &mut self.mfrc522
    }

    pub fn from_i2c(i2c: I2C, address: u8) -> Result<Self, Mfrc522Error<I2C::Error>> {
        let interface = I2cInterface::new(i2c, address);
        let mut mfrc522 = Mfrc522::new(interface).init()?;
        mfrc522.set_antenna_gain(RxGain::DB33)?;
        Ok(Self::new(mfrc522))
    }

    /// Constructor over pure embedded-hal + BoardDesc-derived binding
    /// (t_4864e74e): init, antenna gain, then the VersionReg
    /// self-test — the bring-up sequence moved out of the firmware
    /// mains into the frontend crate. Returns the transceiver and the
    /// version the controller answered with (bench logs stamp it).
    ///
    /// The bus impl arrives already configured for
    /// `binding.bus_hz` — the constructor itself only needs the
    /// address (the run speed is a bus-construction fact, kept in the
    /// binding so one derived struct covers the seam).
    pub fn bring_up(
        i2c: I2C,
        binding: FrontendI2cBinding,
    ) -> Result<(Self, u8), Mfrc522Error<I2C::Error>> {
        let interface = I2cInterface::new(i2c, binding.i2c_addr);
        let mut mfrc522 = Mfrc522::new(interface).init()?;
        mfrc522.set_antenna_gain(RxGain::DB33)?;
        let version = mfrc522.version()?;
        if !VERSION_REG_EXPECTED.contains(&version) {
            // Release the bus back so the caller can probe/retry —
            // same escape hatch as `release()`.
            let _ = mfrc522.release().release();
            return Err(Mfrc522Error::Nak);
        }
        Ok((Self::new(mfrc522), version))
    }

    /// Stateless self-test: the controller answers VersionReg with a
    /// known part number (the same probe `bring_up` gates on).
    pub fn self_test(&mut self) -> Result<u8, Mfrc522Error<I2C::Error>> {
        let version = self.mfrc522.version()?;
        if VERSION_REG_EXPECTED.contains(&version) {
            Ok(version)
        } else {
            Err(Mfrc522Error::Nak)
        }
    }

    pub fn release(self) -> I2C {
        self.mfrc522.release().release()
    }

    /// Maximum number of register polls to wait for the MFRC522 PowerDown bit
    /// to clear during a soft reset. The MFRC522 datasheet specifies the reset
    /// completes within a few milliseconds. On a 100 kHz I2C bus, each poll
    /// takes ~0.3 ms, so 1024 polls ≈ 300 ms — well beyond any expected reset
    /// time. If this limit is exceeded the chip is in an unrecoverable state.
    const SOFT_RESET_MAX_POLLS: usize = 1024;

    fn soft_reset(&mut self) -> Result<(), Mfrc522Error<I2C::Error>> {
        self.mfrc522.write_register(Register::CommandReg, 0x0F)?;
        for _ in 0..Self::SOFT_RESET_MAX_POLLS {
            if self.mfrc522.read_register(Register::CommandReg)? & 0x10 == 0 {
                return Ok(());
            }
        }
        Err(Mfrc522Error::Timeout)
    }

    pub fn reset_frontend(&mut self) -> Result<(), Mfrc522Error<I2C::Error>> {
        self.mfrc522.write_register(Register::TxModeReg, 0x00)?;
        self.mfrc522.write_register(Register::RxModeReg, 0x00)?;
        self.mfrc522.write_register(Register::ModWidthReg, 0x26)?;
        self.mfrc522.write_register(Register::TxASKReg, 0x40)?;
        self.mfrc522
            .write_register(Register::ModeReg, (0x3f & !0b11) | 0b01)?;
        Ok(())
    }

    pub fn prepare_for_activation(&mut self) -> Result<(), Mfrc522Error<I2C::Error>> {
        self.prepare_for_activation_with_delays(&mut NoDelay)
    }

    /// Soft reset + frontend reset + antenna cycle with field-off dwell
    /// ([`FIELD_OFF_DWELL_MS`]) and field-on settle ([`FIELD_ON_SETTLE_MS`])
    /// spent through `delay`. On hardware pass a scheduler-yielding delay
    /// (e.g. `esp_idf_hal::delay::FreeRtos`), never a busy-wait.
    pub fn prepare_for_activation_with_delays<D: DelayNs>(
        &mut self,
        delay: &mut D,
    ) -> Result<(), Mfrc522Error<I2C::Error>> {
        self.soft_reset()?;
        self.reset_frontend()?;
        self.mfrc522
            .rmw_register(Register::TxControlReg, |b| b & !0b11)?;
        delay.delay_ms(FIELD_OFF_DWELL_MS);
        for _ in 0..4 {
            self.mfrc522.write_register(Register::FIFOLevelReg, 0x80)?;
        }
        self.mfrc522
            .rmw_register(Register::TxControlReg, |b| b | 0b11)?;
        delay.delay_ms(FIELD_ON_SETTLE_MS);
        self.mfrc522.set_antenna_gain(RxGain::DB33)?;
        Ok(())
    }

    /// Timer input frequency (MFRC522 datasheet §8.5.2: f_timer =
    /// 13.56 MHz / (2*TPrescaler + 1)).
    const TIMER_INPUT_HZ: u32 = 13_560_000;

    /// Historical fixed prescaler (init()'s Arduino-default heritage):
    /// divisor 2*169+1 = 339 → 40 kHz tick. `set_timeout_ms` never ticks
    /// FASTER than this, so every request expressible under the old
    /// mapping maps to byte-identical register writes.
    const TIMER_MIN_DIVISOR: u32 = 2 * 0xA9 + 1;

    /// Hardware maximum: TPrescaler is 12 bits (TModeReg[3:0] high
    /// nibble + TPrescalerReg low byte — MFRC522 datasheet §8.5.2;
    /// confirmed by esp-idf-rc522 `rc522_pcd_configure_timer`
    /// (`(prescaler >> 8) & 0x0F` into TModeReg) and NuttX
    /// ("TPreScaler = TModeReg[3..0]:TPrescalerReg")), so the divisor
    /// 2*TPrescaler+1 tops out at 8191 → 65535 reload ticks ≈ 39.6 s.
    const TIMER_MAX_DIVISOR: u32 = 2 * 4095 + 1;

    /// Configure the transceive timeout timer.
    ///
    /// (bolty-rs #180, fixed at the canonical home:) the previous fixed
    /// 0xA9 prescaler (40 kHz) capped the 16-bit reload at 1638 ms —
    /// ISO 14443 FWI 13/14 cards (FWT 2483/4957 ms) were silently
    /// clamped and timed out early. The prescaler is now chosen per
    /// request: the SMALLEST divisor ≥ the historical 339 whose tick
    /// keeps the reload within 16 bits (preserving the exact previous
    /// register bytes for every previously-expressible timeout), moving
    /// up to the 12-bit prescaler range for longer waits. Requests
    /// beyond the ~39.6 s hardware maximum are a LOUD error — a silent
    /// clamp was the bug.
    ///
    /// reload = ceil(ms · f_timer / 1000) (round UP so the armed period
    /// is never shorter than requested by tick truncation).
    pub fn set_timeout_ms(&mut self, ms: u32) -> Result<(), Mfrc522Error<I2C::Error>> {
        // Smallest divisor with reload <= 0xFFFF:
        //   ms * f_in / div / 1000 <= 65535  ⇔  div >= ms * f_in / 65_535_000
        // (13560000/65535000 = 904/4369 in lowest terms — u32-safe for
        // any ms; larger requests saturate and fail the max-divisor gate.)
        let min_div = (ms.saturating_mul(904).saturating_add(4_368)) / 4_369;
        let mut div = min_div.max(Self::TIMER_MIN_DIVISOR);
        if div % 2 == 0 {
            div += 1; // 2*TPrescaler+1 is odd; round UP (slower tick keeps reload in range)
        }
        if div > Self::TIMER_MAX_DIVISOR {
            return Err(Mfrc522Error::Timeout);
        }
        let prescaler = (div - 1) / 2; // 0..=4095, 12-bit
        debug_assert!(prescaler <= 0x0FFF);
        // reload = ceil(ms · f_in / (1000 · div)), computed against the
        // TRUE divisor (u64: ms · 13.56 MHz overflows u32 from ~317 ms) so
        // the armed period is never shorter than the request by tick
        // truncation. min_div guarantees the result fits 16 bits.
        let reload = ((u64::from(ms) * u64::from(Self::TIMER_INPUT_HZ) + u64::from(div) * 1000 - 1)
            / (u64::from(div) * 1000)) as u32;
        debug_assert!(reload <= 0xFFFF);

        self.mfrc522
            .write_register(Register::TModeReg, 0x80 | ((prescaler >> 8) as u8 & 0x0F))?;
        self.mfrc522
            .write_register(Register::TPrescalerReg, prescaler as u8)?;
        self.mfrc522
            .write_register(Register::TReloadRegHigh, (reload >> 8) as u8)?;
        self.mfrc522
            .write_register(Register::TReloadRegLow, reload as u8)?;
        Ok(())
    }

    pub fn enable_hw_crc(&mut self) -> Result<(), Mfrc522Error<I2C::Error>> {
        self.mfrc522
            .rmw_register(Register::TxModeReg, |v| v | 0x80)?;
        self.mfrc522
            .rmw_register(Register::RxModeReg, |v| v | 0x80)?;
        Ok(())
    }
}

impl<I2C: I2c> PcdTransceiver for Mfrc522Transceiver<I2C> {
    type Error = Mfrc522Error<I2C::Error>;

    fn transceive(&mut self, frame: &Frame) -> Result<FrameVec, Self::Error> {
        self.mfrc522
            .rmw_register(Register::CollReg, |value| value & !0x80)?;

        let response = match frame {
            Frame::Short(data) => {
                let fifo = self.mfrc522.transceive::<2>(data.as_slice(), 7, 0)?;
                if fifo.valid_bits != 0 {
                    return Err(Mfrc522Error::Protocol);
                }
                let len = fifo.valid_bytes.min(fifo.buffer.len());
                fifo.buffer
                    .get(..len)
                    .ok_or(Mfrc522Error::BufferOverflow)?
                    .to_vec()
            }
            Frame::BitOriented(data) => {
                let tx_last_bits = data.get(1).copied().unwrap_or_default() & 0x07;
                let fifo =
                    self.mfrc522
                        .transceive::<5>(data.as_slice(), tx_last_bits, tx_last_bits)?;
                if fifo.valid_bits != 0 {
                    return Err(Mfrc522Error::Protocol);
                }
                let len = fifo.valid_bytes.min(fifo.buffer.len());
                fifo.buffer
                    .get(..len)
                    .ok_or(Mfrc522Error::BufferOverflow)?
                    .to_vec()
            }
            Frame::Standard(data) => {
                let fifo = match self.mfrc522.transceive::<64>(data.as_slice(), 0, 0) {
                    Ok(f) => f,
                    Err(Mfrc522Error::Crc) => {
                        log_crc_error(&mut self.mfrc522);
                        return Err(Mfrc522Error::Crc);
                    }
                    Err(e) => return Err(e),
                };
                if fifo.valid_bits != 0 {
                    return Err(Mfrc522Error::Protocol);
                }
                let len = fifo.valid_bytes.min(fifo.buffer.len());
                fifo.buffer
                    .get(..len)
                    .ok_or(Mfrc522Error::BufferOverflow)?
                    .to_vec()
            }
        };

        Ok(response)
    }

    fn try_enable_hw_crc(&mut self) -> Result<(), Self::Error> {
        Err(Mfrc522Error::Protocol)
    }

    fn try_set_timeout_ms(&mut self, ms: u32) -> Result<(), ()> {
        self.set_timeout_ms(ms).map_err(|_| ())
    }
}

fn log_crc_error<I2C: I2c>(mfrc522: &mut Mfrc522<I2cInterface<I2C>, Initialized>) {
    let irq = mfrc522.read_register(Register::ComIrqReg).unwrap_or(0xFF);
    let err = mfrc522.read_register(Register::ErrorReg).unwrap_or(0xFF);
    let level = mfrc522.read_register(Register::FIFOLevelReg).unwrap_or(0) as usize;
    log::warn!(
        "MFRC522 CRC error: ComIrq=0x{:02X} Err=0x{:02X} FIFO={} bytes",
        irq,
        err,
        level
    );
}

#[cfg(target_arch = "xtensa")]
pub mod bus_recovery;

#[cfg(target_arch = "xtensa")]
pub use bus_recovery::{BusRecovery, recover_i2c_bus};

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        // Register-file indexing: every index is a compile-time MFRC522
        // register constant < 0x40 — the panicking case is unrepresentable.
        clippy::indexing_slicing,
        clippy::string_slice
    )]

    /// #257 thiserror conversion lock: the derived Display must stay
    /// byte-identical to the hand-rolled strings it replaced.
    #[test]
    fn error_display_strings_are_locked() {
        use super::Error;
        use alloc::string::ToString as _;
        assert_eq!(
            Error::<u8>::Communication(7).to_string(),
            "MFRC522 communication error: 7"
        );
        assert_eq!(Error::<u8>::Timeout.to_string(), "MFRC522 timeout");
        assert_eq!(Error::<u8>::Protocol.to_string(), "MFRC522 protocol error");
        assert_eq!(
            Error::<u8>::BufferOverflow.to_string(),
            "MFRC522 buffer overflow"
        );
        let _: &dyn core::error::Error = &Error::<u8>::Timeout;
    }

    use super::*;
    use alloc::vec::Vec;
    use core::convert::Infallible;
    use embedded_hal::i2c::Operation;

    fn reg_index(reg: u8) -> usize {
        usize::from(reg)
    }

    // Register addresses (MFRC522 datasheet chapter 9; mirrored here so the
    // tests can assert against the mock's write log without importing the
    // enum's discriminants indirectly).
    const COMMAND_REG: u8 = 0x01;
    const FIFO_LEVEL_REG: u8 = 0x0A;
    const MODE_REG: u8 = 0x11;
    const TX_MODE_REG: u8 = 0x12;
    const RX_MODE_REG: u8 = 0x13;
    const TX_CONTROL_REG: u8 = 0x14;
    const MOD_WIDTH_REG: u8 = 0x24;
    const RF_CFG_REG: u8 = 0x26;
    const T_MODE_REG: u8 = 0x2A;
    const T_PRESCALER_REG: u8 = 0x2B;
    const T_RELOAD_HIGH_REG: u8 = 0x2C;
    const T_RELOAD_LOW_REG: u8 = 0x2D;

    /// Stateful MFRC522 register-file mock: single-register writes persist,
    /// reads return the stored byte, and every write is recorded so tests
    /// can assert the exact register program order.
    #[derive(Debug)]
    struct MockI2c {
        regs: [u8; 0x40],
        reg_pointer: u8,
        writes: Vec<(u8, u8)>,
        reads: Vec<u8>,
        command_reads: usize,
        /// Wedge CommandReg into permanent PowerDown after this many reads
        /// of it — must exceed the one read mfrc522's own `init()` reset
        /// does (that loop is UNBOUNDED upstream, so the wedge may only
        /// start afterwards; bolty's soft_reset is the bounded one).
        power_down_stuck_after: usize,
    }

    impl Default for MockI2c {
        fn default() -> Self {
            Self {
                regs: [0u8; 0x40],
                reg_pointer: 0,
                writes: Vec::new(),
                reads: Vec::new(),
                command_reads: 0,
                power_down_stuck_after: usize::MAX,
            }
        }
    }

    impl MockI2c {
        /// Mock whose CommandReg wedges into PowerDown after `after`
        /// further reads of it (must exceed init()'s one read — mfrc522's
        /// upstream reset poll is UNBOUNDED; only bolty's bounded soft_reset
        /// may observe the wedge).
        fn wedged_after(after: usize) -> Self {
            Self {
                power_down_stuck_after: after,
                ..Self::default()
            }
        }

        fn reg(&self, reg: u8) -> u8 {
            self.regs[reg_index(reg)]
        }

        fn writes_to(&self, reg: u8) -> Vec<u8> {
            self.writes
                .iter()
                .filter(|(r, _)| *r == reg)
                .map(|(_, v)| *v)
                .collect()
        }

        fn last_write_to(&self, reg: u8) -> u8 {
            *self
                .writes
                .iter()
                .filter(|(r, _)| *r == reg)
                .map(|(_, v)| v)
                .next_back()
                .unwrap_or(&0)
        }

        fn read_reg(&mut self, reg: u8, buf: &mut [u8]) {
            self.reads.push(reg);
            let wedged = reg == COMMAND_REG && self.command_reads >= self.power_down_stuck_after;
            if reg == COMMAND_REG {
                self.command_reads += 1;
            }
            if wedged {
                buf.fill(0x10);
            } else {
                buf.fill(self.regs[reg_index(reg)]);
            }
        }
    }

    impl embedded_hal::i2c::ErrorType for MockI2c {
        type Error = Infallible;
    }

    impl I2c for MockI2c {
        fn transaction(
            &mut self,
            _address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            // eh-1.0's provided write_read/read/write are single transaction
            // calls with these exact operation shapes.
            match operations {
                [Operation::Write(w)] if w.len() == 2 => {
                    self.regs[usize::from(w[0])] = w[1];
                    self.writes.push((w[0], w[1]));
                }
                [Operation::Write(w), Operation::Read(buf)] if w.len() == 1 => {
                    let reg = w[0];
                    self.reg_pointer = reg;
                    self.read_reg(reg, buf);
                }
                [Operation::Write(w)] if w.len() == 1 => {
                    self.reg_pointer = w[0];
                }
                [Operation::Read(buf)] => {
                    let reg = self.reg_pointer;
                    self.read_reg(reg, buf);
                }
                _ => {}
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingDelay {
        ms_calls: Vec<u32>,
    }

    impl DelayNs for RecordingDelay {
        fn delay_ns(&mut self, _ns: u32) {}
        fn delay_ms(&mut self, ms: u32) {
            self.ms_calls.push(ms);
        }
    }

    #[test]
    fn no_delay_is_zero_cost() {
        let mut d = NoDelay;
        d.delay_ms(FIELD_OFF_DWELL_MS);
        d.delay_us(1);
        d.delay_ns(1);
    }

    #[test]
    fn fn_delay_rounds_ns_up_to_whole_ms() {
        use core::sync::atomic::{AtomicU32, Ordering};
        static MS: AtomicU32 = AtomicU32::new(0);
        fn record(ms: u32) {
            MS.fetch_add(ms, Ordering::SeqCst);
        }
        let mut d = FnDelay(record);
        d.delay_ns(1);
        assert_eq!(MS.load(Ordering::SeqCst), 1);
        d.delay_ms(50);
        assert_eq!(MS.load(Ordering::SeqCst), 51);
    }

    #[test]
    fn dwell_constants_match_bench_proven_values() {
        assert_eq!(FIELD_OFF_DWELL_MS, 50);
        assert_eq!(FIELD_ON_SETTLE_MS, 5);
    }

    #[test]
    fn prepare_with_delays_spends_dwell_then_settle() {
        let mock = MockI2c::default();
        let mut t = Mfrc522Transceiver::from_i2c(mock, DEFAULT_I2C_ADDRESS).unwrap();
        let mut delay = RecordingDelay::default();
        t.prepare_for_activation_with_delays(&mut delay).unwrap();
        assert_eq!(
            delay.ms_calls,
            alloc::vec![FIELD_OFF_DWELL_MS, FIELD_ON_SETTLE_MS]
        );
    }

    #[test]
    fn legacy_prepare_never_delays() {
        let mock = MockI2c::default();
        let mut t = Mfrc522Transceiver::from_i2c(mock, DEFAULT_I2C_ADDRESS).unwrap();
        t.prepare_for_activation().unwrap();
    }

    #[test]
    fn prepare_programs_frontend_reset_then_antenna_cycle() {
        let mock = MockI2c::default();
        let mut t = Mfrc522Transceiver::from_i2c(mock, DEFAULT_I2C_ADDRESS).unwrap();
        let mut delay = RecordingDelay::default();
        t.prepare_for_activation_with_delays(&mut delay).unwrap();

        let i2c = t.release();
        assert_eq!(i2c.last_write_to(TX_MODE_REG), 0x00);
        assert_eq!(i2c.last_write_to(RX_MODE_REG), 0x00);
        assert_eq!(i2c.last_write_to(MOD_WIDTH_REG), 0x26);
        assert_eq!(i2c.last_write_to(MODE_REG), (0x3f & !0b11) | 0b01);
        assert_eq!(
            i2c.writes_to(FIFO_LEVEL_REG),
            alloc::vec![0x80, 0x80, 0x80, 0x80]
        );
        let gain_writes = i2c.writes_to(RF_CFG_REG);
        assert!(
            gain_writes.contains(&0x40),
            "RxGain::DB33 not applied: {gain_writes:?}"
        );
        assert!(
            i2c.reads.contains(&TX_CONTROL_REG),
            "TxControlReg rmw must read before write"
        );
    }

    #[test]
    fn soft_reset_poll_is_bounded_when_power_down_never_clears() {
        // Wedge after init()'s single CommandReg read: only bolty's bounded
        // soft_reset poll may observe the wedge.
        let mut t =
            Mfrc522Transceiver::from_i2c(MockI2c::wedged_after(1), DEFAULT_I2C_ADDRESS).unwrap();
        let err = t.prepare_for_activation().unwrap_err();
        assert!(matches!(err, Mfrc522Error::Timeout));
        let i2c = t.release();
        let polls = i2c.reads.iter().filter(|&&r| r == COMMAND_REG).count();
        assert_eq!(
            polls,
            Mfrc522Transceiver::<MockI2c>::SOFT_RESET_MAX_POLLS + 1
        );
    }

    #[test]
    fn set_timeout_ms_maps_ms_onto_timer_registers() {
        let mock = MockI2c::default();
        let mut t = Mfrc522Transceiver::from_i2c(mock, DEFAULT_I2C_ADDRESS).unwrap();

        // f_timer = 13.56 MHz / (2*0xA9 + 1) = 40 kHz → 50 ms = 2000 = 0x07D0
        // (deliberately differs from init()'s 100 ms Arduino default so the
        // test proves OUR mapping, not init's). Requests the historical
        // 0xA9 prescaler can express map to BYTE-IDENTICAL register writes
        // — the dynamic-prescaler fix must not perturb them.
        t.set_timeout_ms(50).unwrap();
        let i2c = t.release();
        assert_eq!(i2c.last_write_to(T_MODE_REG), 0x80);
        assert_eq!(i2c.last_write_to(T_PRESCALER_REG), 0xA9);
        assert_eq!(i2c.last_write_to(T_RELOAD_HIGH_REG), 0x07);
        assert_eq!(i2c.last_write_to(T_RELOAD_LOW_REG), 0xD0);
    }

    #[test]
    fn set_timeout_ms_boundary_stays_on_historical_prescaler() {
        // 1638 ms is the last request expressible at the 0xA9/40 kHz tick
        // (reload 65520 <= 0xFFFF); the next millisecond must move the
        // prescaler, not clamp the reload.
        let mock = MockI2c::default();
        let mut t = Mfrc522Transceiver::from_i2c(mock, DEFAULT_I2C_ADDRESS).unwrap();
        t.set_timeout_ms(1638).unwrap();
        let i2c = t.release();
        assert_eq!(i2c.last_write_to(T_MODE_REG), 0x80);
        assert_eq!(i2c.last_write_to(T_PRESCALER_REG), 0xA9);
        assert_eq!(i2c.last_write_to(T_RELOAD_HIGH_REG), (65_520u32 >> 8) as u8);
        assert_eq!(i2c.last_write_to(T_RELOAD_LOW_REG), 65_520u32 as u8);
    }

    #[test]
    fn set_timeout_ms_fwi13_2483ms_widens_prescaler() {
        // bolty-rs #180: FWI 13 FWT = 2483 ms — previously SILENTLY
        // clamped to 1638 ms at the fixed 0xA9 tick. Expected mapping:
        // divisor 515 (= 2*257+1) → 12-bit prescaler 0x101 (TModeReg
        // high nibble 0x1, TPrescalerReg 0x01), f_timer = 26,330 Hz,
        // reload = 65,378 = 0xFF62 (armed period 2.4830 s ≥ request).
        let mock = MockI2c::default();
        let mut t = Mfrc522Transceiver::from_i2c(mock, DEFAULT_I2C_ADDRESS).unwrap();
        t.set_timeout_ms(2483).unwrap();
        let i2c = t.release();
        assert_eq!(
            i2c.last_write_to(T_MODE_REG),
            0x80 | 0x01,
            "TPrescaler high nibble"
        );
        assert_eq!(i2c.last_write_to(T_PRESCALER_REG), 0x01);
        assert_eq!(i2c.last_write_to(T_RELOAD_HIGH_REG), 0xFF);
        assert_eq!(i2c.last_write_to(T_RELOAD_LOW_REG), 0x62);
    }

    #[test]
    fn set_timeout_ms_fwi14_4957ms_widens_prescaler() {
        // bolty-rs #180: FWI 14 FWT = 4957 ms. Expected mapping:
        // divisor 1027 (= 2*513+1) → 12-bit prescaler 0x201 (TModeReg
        // high nibble 0x2, TPrescalerReg 0x01), f_timer = 13,203 Hz,
        // reload = 65,450 = 0xFFAA (armed period 4.9583 s ≥ request).
        let mock = MockI2c::default();
        let mut t = Mfrc522Transceiver::from_i2c(mock, DEFAULT_I2C_ADDRESS).unwrap();
        t.set_timeout_ms(4957).unwrap();
        let i2c = t.release();
        assert_eq!(
            i2c.last_write_to(T_MODE_REG),
            0x80 | 0x02,
            "TPrescaler high nibble"
        );
        assert_eq!(i2c.last_write_to(T_PRESCALER_REG), 0x01);
        assert_eq!(i2c.last_write_to(T_RELOAD_HIGH_REG), 0xFF);
        assert_eq!(i2c.last_write_to(T_RELOAD_LOW_REG), 0xAA);
    }

    #[test]
    fn set_timeout_ms_rejects_beyond_hardware_maximum() {
        // ~39.6 s is the 12-bit-prescaler + 16-bit-reload ceiling; a
        // silent clamp was the #180 bug — over-max requests now error
        // LOUDLY and touch no timer registers.
        let mock = MockI2c::default();
        let mut t = Mfrc522Transceiver::from_i2c(mock, DEFAULT_I2C_ADDRESS).unwrap();
        let err = t.set_timeout_ms(60_000).unwrap_err();
        assert!(matches!(err, Mfrc522Error::Timeout));
        let err = t.set_timeout_ms(u32::MAX).unwrap_err();
        assert!(matches!(err, Mfrc522Error::Timeout));
        let i2c = t.release();
        // init()'s Arduino-default timer registers, untouched by the
        // rejected calls (TModeReg 0x80 / 0xA9 / reload 0x0FA0).
        assert_eq!(
            i2c.last_write_to(T_MODE_REG),
            0x80,
            "no timer registers written"
        );
        assert_eq!(i2c.last_write_to(T_PRESCALER_REG), 0xA9);
        assert_eq!(i2c.last_write_to(T_RELOAD_HIGH_REG), 0x0F);
        assert_eq!(i2c.last_write_to(T_RELOAD_LOW_REG), 0xA0);
    }

    // ---- t_4864e74e: constructor over pure embedded-hal + the
    // ---- BoardDesc-derived binding, with the VersionReg self-test.

    const VERSION_REG: u8 = 0x37;

    fn healthy_mock() -> MockI2c {
        let mut mock = MockI2c::default();
        // A healthy MFRC522 answers VersionReg with 0x92 (both 0x91
        // and 0x92 are expected parts — datasheet table 19).
        mock.regs[reg_index(VERSION_REG)] = 0x92;
        mock
    }

    #[test]
    fn binding_carries_the_board_derived_numbers() {
        let b = FrontendI2cBinding::new(0x28, 400_000);
        assert_eq!(b.i2c_addr, DEFAULT_I2C_ADDRESS);
        assert_eq!(b.bus_hz, 400_000);
    }

    #[test]
    fn bring_up_initializes_and_gates_on_the_version_self_test() {
        let (mut t, version) =
            Mfrc522Transceiver::bring_up(healthy_mock(), FrontendI2cBinding::new(0x28, 400_000))
                .unwrap();
        assert_eq!(version, 0x92);
        // Constructed healthy: the same probe passes again.
        assert_eq!(t.self_test().unwrap(), 0x92);
    }

    #[test]
    fn bring_up_rejects_an_unexpected_version() {
        // Default mock regs: VersionReg reads 0x00 — no controller
        // (or wrong part) answers. The constructor must fail loudly.
        let err = Mfrc522Transceiver::bring_up(
            MockI2c::default(),
            FrontendI2cBinding::new(0x28, 400_000),
        )
        .err()
        .expect("unexpected version must fail bring_up");
        assert!(matches!(err, Mfrc522Error::Nak));
    }

    #[test]
    fn self_test_flags_a_drifted_version() {
        let mut mock = healthy_mock();
        let mut t = Mfrc522Transceiver::from_i2c(mock, DEFAULT_I2C_ADDRESS).unwrap();
        assert_eq!(t.self_test().unwrap(), 0x92);
        mock = t.release();
        mock.regs[reg_index(VERSION_REG)] = 0x00;
        let mut t = Mfrc522Transceiver::from_i2c(mock, DEFAULT_I2C_ADDRESS).unwrap();
        assert!(matches!(t.self_test().unwrap_err(), Mfrc522Error::Nak));
    }

    #[test]
    fn mock_registers_roundtrip() {
        let mut mock = MockI2c::default();
        mock.regs[usize::from(TX_CONTROL_REG)] = 0b11;
        let mut t = Mfrc522Transceiver::from_i2c(mock, DEFAULT_I2C_ADDRESS).unwrap();
        let antenna_on = t.raw().read_register(Register::TxControlReg).unwrap();
        assert_eq!(antenna_on & 0b11, 0b11);
        let i2c = t.release();
        assert_eq!(i2c.reg(TX_CONTROL_REG) & 0b11, 0b11);
    }
}
