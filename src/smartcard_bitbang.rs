#![cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
#![allow(dead_code)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::needless_range_loop)]

use core::convert::Infallible;

use crate::pps_fsm::{di_from_ta1, fi_from_ta1, PpsFsm, PpsState};
use crate::t1_engine::{transmit_apdu_t1, T1Error, T1Transport};
use cortex_m::peripheral::DCB;
use stm32f7xx_hal::gpio::{
    gpiof::{PF10, PF6, PF7},
    gpioi::{PI0, PI2},
    Input, OpenDrain, Output, PushPull,
};
use stm32f7xx_hal::pac::{gpiof, gpioi, tim10, DWT, GPIOF, GPIOI, RCC, TIM10};

const SYSCLK_HZ: u32 = 216_000_000;
const CARD_CLK_HZ: u32 = 1_000_000;
const FI_DEFAULT: u32 = 372;
const DI_DEFAULT: u32 = 1;
const ETU_CPU_CYCLES: u32 = (SYSCLK_HZ / CARD_CLK_HZ) * FI_DEFAULT / DI_DEFAULT; // 80,352
const GPIO_CLK_HALF_CYCLES: u32 = SYSCLK_HZ / (CARD_CLK_HZ * 2); // 108

const SC_ATR_TIMEOUT_CYCLES: u32 = SYSCLK_HZ * 4; // 4 seconds
const SC_ATR_BYTE_TIMEOUT_CYCLES: u32 = SYSCLK_HZ / 25; // 40 ms
const SC_BYTE_TIMEOUT_CYCLES: u32 = SYSCLK_HZ / 25; // 40 ms
const SC_PROCEDURE_TIMEOUT_CYCLES: u32 = SYSCLK_HZ; // 1000 ms
const SC_T0_GET_RESPONSE_MAX: u8 = 32;
const SC_ATR_MAX_LEN: usize = 33;
const SC_POWER_ON_DELAY_MS: u32 = 50;
const SC_CLK_TO_RST_DELAY_CARD_CLKS: u32 = 50_000;

// ---------------------------------------------------------------------------
// Diagnostic ring buffer — zero-overhead RAM log exposed via CCID Escape.
// TLV format: [tag:1][len:1][data:len].  Max 261 bytes (fits one CCID msg).
// ---------------------------------------------------------------------------
const DIAG_BUF_SIZE: usize = 261;

#[cfg(feature = "stm32f746")]
static mut DIAG_BUF: [u8; DIAG_BUF_SIZE] = [0; DIAG_BUF_SIZE];
#[cfg(feature = "stm32f746")]
static mut DIAG_LEN: usize = 0;

const DTAG_IO_READBACK: u8 = 0x01;
const DTAG_ATR: u8 = 0x02;
const DTAG_TX_SINGLE: u8 = 0x03;
const DTAG_TX_BYTE_ERR: u8 = 0x04;
const DTAG_DWT_STAMP: u8 = 0x05;
const DTAG_TIM10_REGS: u8 = 0x06;
const DTAG_END: u8 = 0xFF;

#[cfg(feature = "stm32f746")]
fn diag_clear() {
    unsafe {
        DIAG_BUF = [0; DIAG_BUF_SIZE];
        DIAG_LEN = 0;
    }
}

#[inline(always)]
#[cfg(feature = "stm32f746")]
fn diag_push(byte: u8) {
    unsafe {
        if DIAG_LEN < DIAG_BUF.len() {
            DIAG_BUF[DIAG_LEN] = byte;
            DIAG_LEN += 1;
        }
    }
}

#[cfg(feature = "stm32f746")]
fn diag_tlv(tag: u8, data: &[u8]) {
    if data.len() > 255 {
        return;
    }
    diag_push(tag);
    diag_push(data.len() as u8);
    for &b in data {
        diag_push(b);
    }
}

#[cfg(feature = "stm32f746")]
fn diag_end() {
    diag_push(DTAG_END);
    diag_push(0);
}

#[cfg(feature = "stm32f746")]
pub fn read_diag() -> &'static [u8] {
    unsafe { &DIAG_BUF[..DIAG_LEN] }
}

#[cfg(feature = "stm32f746")]
pub fn seal_diag() {
    let cyccnt = unsafe { (*DWT::PTR).cyccnt.read() };
    diag_tlv(DTAG_DWT_STAMP, &cyccnt.to_le_bytes());
    diag_end();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum SmartcardError {
    NoCard,
    Timeout,
    InvalidATR,
    ParityError,
    ProtocolError,
    BufferOverflow,
    HardwareError,
}

impl From<Infallible> for SmartcardError {
    fn from(_: Infallible) -> Self {
        SmartcardError::HardwareError
    }
}

struct Atr {
    raw: [u8; SC_ATR_MAX_LEN],
    len: usize,
}

impl Default for Atr {
    fn default() -> Self {
        Self {
            raw: [0; SC_ATR_MAX_LEN],
            len: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AtrParams {
    pub fi: u16,
    pub di: u8,
    pub ta1: u8,
    pub protocol: u8,
    pub guard_time_n: u8,
    pub ifsc: u8,
    pub bwi: u8,
    pub cwi: u8,
    pub edc_type: u8,
    pub has_ta1: bool,
}

pub fn parse_atr(atr: &[u8]) -> AtrParams {
    let mut p = AtrParams {
        fi: 372,
        di: 1,
        ifsc: 32,
        bwi: 4,
        cwi: 13,
        ..AtrParams::default()
    };
    if atr.len() < 2 {
        return p;
    }
    let t0 = atr[1];
    let mut y = (t0 >> 4) & 0x0F;
    let mut idx = 2usize;
    let mut level = 1u8;
    let mut td_protocol: u8 = 0;

    loop {
        if (y & 0x01) != 0 {
            if idx >= atr.len() {
                break;
            }
            let ta = atr[idx];
            idx += 1;
            if level == 1 {
                p.ta1 = ta;
                p.has_ta1 = true;
                p.fi = fi_from_ta1(ta);
                p.di = di_from_ta1(ta);
            } else if level >= 3 && td_protocol == 1 {
                p.ifsc = ta;
            }
        }
        if (y & 0x02) != 0 {
            if idx >= atr.len() {
                break;
            }
            let tb = atr[idx];
            idx += 1;
            if level >= 2 && td_protocol == 1 {
                p.bwi = (tb >> 4) & 0x0F;
                p.cwi = tb & 0x0F;
            }
        }
        if (y & 0x04) != 0 {
            if idx >= atr.len() {
                break;
            }
            let tc = atr[idx];
            idx += 1;
            if level == 1 {
                p.guard_time_n = tc;
            } else if td_protocol == 1 {
                p.edc_type = tc & 1;
            }
        }
        if (y & 0x08) != 0 {
            if idx >= atr.len() {
                break;
            }
            let td = atr[idx];
            idx += 1;
            td_protocol = td & 0x0F;
            if level == 1 {
                p.protocol = td_protocol;
            }
            y = (td >> 4) & 0x0F;
            level += 1;
        } else {
            break;
        }
    }
    p
}

pub struct SmartcardBitbang {
    io_pin: PI0<Output<OpenDrain>>,
    clk_pin: PF6<Output<PushPull>>,
    rst_pin: PI2<Output<PushPull>>,
    pres_pin: PF10<Input>,
    pwr_pin: PF7<Output<PushPull>>,
    atr: Atr,
    powered: bool,
    protocol: u8,
    ifsc: u8,
    t1_ns: u8,
    etu_cycles: u32,
    clock_running: bool,
}

impl SmartcardBitbang {
    pub fn new(
        mut io_pin: PI0<Output<OpenDrain>>,
        clk_pin: PF6<Output<PushPull>>,
        mut rst_pin: PI2<Output<PushPull>>,
        pres_pin: PF10<Input>,
        mut pwr_pin: PF7<Output<PushPull>>,
        _sysclk_hz: u32,
    ) -> Self {
        Self::enable_dwt();
        io_pin.set_high();
        rst_pin.set_low();
        pwr_pin.set_high();
        Self {
            io_pin,
            clk_pin,
            rst_pin,
            pres_pin,
            pwr_pin,
            atr: Atr::default(),
            powered: false,
            protocol: 0,
            ifsc: 32,
            t1_ns: 0,
            etu_cycles: ETU_CPU_CYCLES,
            clock_running: false,
        }
    }

    fn enable_dwt() {
        unsafe {
            (*DCB::PTR).demcr.modify(|r| r | (1 << 24));
            DWT::unlock();
            (*DWT::PTR).cyccnt.write(0);
            (*DWT::PTR).ctrl.modify(|r| r | 1);
        }
    }

    #[inline(always)]
    fn cyccnt() -> u32 {
        unsafe { (*DWT::PTR).cyccnt.read() }
    }

    fn delay_until(deadline: u32) {
        while deadline.wrapping_sub(Self::cyccnt()) < 0x8000_0000 {}
    }

    fn delay_cycles(n: u32) {
        let deadline = Self::cyccnt().wrapping_add(n);
        Self::delay_until(deadline);
    }

    #[inline(always)]
    fn gpiof_ptr() -> &'static gpiof::RegisterBlock {
        unsafe { &*GPIOF::ptr() }
    }

    #[inline(always)]
    fn gpioi_ptr() -> &'static gpioi::RegisterBlock {
        unsafe { &*GPIOI::ptr() }
    }

    #[inline(always)]
    fn io_is_high(&self) -> bool {
        (Self::gpioi_ptr().idr.read().bits() & (1 << 0)) != 0
    }

    #[inline(always)]
    fn io_drive_low(&mut self) {
        Self::gpioi_ptr()
            .bsrr
            .write(|w| unsafe { w.bits(1 << (0 + 16)) });
    }

    #[inline(always)]
    fn io_release_high(&mut self) {
        Self::gpioi_ptr().bsrr.write(|w| unsafe { w.bits(1 << 0) });
    }

    fn delay_ms(ms: u32) {
        for _ in 0..ms {
            cortex_m::asm::delay(216_000);
        }
    }

    #[inline(always)]
    fn tim10_ptr() -> &'static tim10::RegisterBlock {
        unsafe { &*TIM10::ptr() }
    }

    #[inline(always)]
    fn rcc_ptr() -> &'static stm32f7xx_hal::pac::rcc::RegisterBlock {
        unsafe { &*RCC::ptr() }
    }

    fn start_continuous_clock(&mut self) {
        let gpiof = Self::gpiof_ptr();
        let rcc = Self::rcc_ptr();
        let tim10 = Self::tim10_ptr();

        gpiof.bsrr.write(|w| unsafe { w.bits(1 << 6) });

        // PF6: GPIO → AF mode (10)
        let moder = gpiof.moder.read().bits();
        gpiof
            .moder
            .write(|w| unsafe { w.bits((moder & !(3 << 12)) | (2 << 12)) });

        // PF6: AF3 = TIM10_CH1
        let afrl = gpiof.afrl.read().bits();
        gpiof
            .afrl
            .write(|w| unsafe { w.bits((afrl & !(0xf << 24)) | (3 << 24)) });

        rcc.apb2enr.modify(|_, w| w.tim10en().set_bit());
        cortex_m::asm::delay(10);

        tim10.cr1.write(|w| unsafe { w.bits(0) });

        // TIM10 kernel clock = 216 MHz (APB2=108 MHz × 2 multiplier).
        // Target 1 MHz → PSC=0, ARR=215 (216 MHz / 216 = 1 MHz).
        tim10.psc.write(|w| unsafe { w.bits(0) });
        tim10.arr.write(|w| unsafe { w.bits(215) });
        tim10.ccr1().write(|w| unsafe { w.bits(107) }); // 50% duty

        tim10.ccmr1_output().write(|w| {
            w.oc1m().pwm_mode1();
            w.oc1pe().set_bit();
            w
        });

        tim10.ccer.write(|w| w.cc1e().set_bit());
        tim10.cr1.write(|w| unsafe { w.bits((1 << 7) | 1) }); // ARPE + CEN
        self.clock_running = true;
    }

    fn stop_continuous_clock(&mut self) {
        if !self.clock_running {
            return;
        }
        let gpiof = Self::gpiof_ptr();
        let tim10 = Self::tim10_ptr();

        tim10.cr1.write(|w| unsafe { w.bits(0) });
        tim10.ccer.write(|w| unsafe { w.bits(0) });

        // PF6: back to GPIO output mode (01)
        let moder = gpiof.moder.read().bits();
        gpiof
            .moder
            .write(|w| unsafe { w.bits((moder & !(3 << 12)) | (1 << 12)) });

        gpiof
            .afrl
            .modify(|r, w| unsafe { w.bits(r.bits() & !(0xf << 24)) });

        gpiof.bsrr.write(|w| unsafe { w.bits(1 << (6 + 16)) });
        self.clock_running = false;
    }

    pub fn is_card_present(&self) -> bool {
        self.pres_pin.is_high()
    }

    pub fn set_protocol(&mut self, protocol: u8) {
        self.protocol = protocol;
    }

    pub fn protocol(&self) -> u8 {
        self.protocol
    }

    pub fn set_clock(&mut self, _enable: bool) {}

    pub fn set_clock_and_rate(
        &mut self,
        _clock_hz: u32,
        _rate_bps: u32,
    ) -> Result<(u32, u32), SmartcardError> {
        Ok((1_000_000, 9600))
    }

    pub fn power_on(&mut self) -> Result<&[u8], SmartcardError> {
        let atr = self.power_on_atr()?;
        Ok(&atr.raw[..atr.len])
    }

    fn power_on_atr(&mut self) -> Result<&Atr, SmartcardError> {
        if !self.is_card_present() {
            return Err(SmartcardError::NoCard);
        }

        diag_clear();

        // Full cold reset: power off, RST low, IO released
        self.stop_continuous_clock();
        self.pwr_pin.set_high(); // VCC off
        self.rst_pin.set_low();
        self.io_release_high();
        Self::delay_ms(200);

        self.atr = Atr::default();
        self.powered = false;
        self.protocol = 0;
        self.ifsc = 32;
        self.t1_ns = 0;
        self.etu_cycles = ETU_CPU_CYCLES;

        self.io_readback_test();

        // Activation: VCC on → start CLK → wait → RST high
        self.pwr_pin.set_low(); // VCC on
        Self::delay_ms(SC_POWER_ON_DELAY_MS);

        // H5 test: GPIO-toggled clock instead of TIM10
        let gpiof = Self::gpiof_ptr();
        for _ in 0..SC_CLK_TO_RST_DELAY_CARD_CLKS {
            gpiof.bsrr.write(|w| unsafe { w.bits(1 << 6) });
            cortex_m::asm::delay(GPIO_CLK_HALF_CYCLES);
            gpiof.bsrr.write(|w| unsafe { w.bits(1 << (6 + 16)) });
            cortex_m::asm::delay(GPIO_CLK_HALF_CYCLES);
        }

        // Start continuous TIM10 PWM clock — card needs clock during ATR + all communication
        self.start_continuous_clock();

        self.rst_pin.set_high(); // Release RST → card sends ATR

        // Mask interrupts only during ATR byte reception
        cortex_m::interrupt::disable();
        match self.read_atr() {
            Ok(()) => {
                self.powered = true;

                let atr_slice = &self.atr.raw[..self.atr.len];
                let params = parse_atr(atr_slice);
                diag_tlv(DTAG_ATR, atr_slice);
                self.detect_protocol_from_atr();

                defmt::info!(
                    "ATR OK: len={} proto={} TA1=0x{:02X} Fi={} Di={} IFSC={}",
                    self.atr.len,
                    params.protocol,
                    params.ta1,
                    params.fi,
                    params.di,
                    params.ifsc
                );

                self.ifsc = params.ifsc;

                unsafe {
                    cortex_m::interrupt::enable();
                }

                if params.protocol == 1 {
                    let _ = self.negotiate_pps_fsm(&params);
                }

                seal_diag();

                Ok(&self.atr)
            }
            Err(e) => {
                unsafe {
                    cortex_m::interrupt::enable();
                }
                Err(e)
            }
        }
    }

    pub fn power_off(&mut self) {
        self.rst_pin.set_low();
        self.stop_continuous_clock();
        self.pwr_pin.set_high();
        self.io_release_high();
        self.powered = false;
        self.atr = Atr::default();
        self.protocol = 0;
        self.ifsc = 32;
        self.t1_ns = 0;
        self.etu_cycles = ETU_CPU_CYCLES;
    }

    fn recv_byte_timeout(&mut self, timeout_cycles: u32) -> Result<u8, SmartcardError> {
        self.io_release_high();
        let etu = self.etu_cycles;
        let gpiof = Self::gpiof_ptr();
        let deadline = Self::cyccnt().wrapping_add(timeout_cycles);

        // Toggle clock while polling for start-bit falling edge
        loop {
            gpiof.bsrr.write(|w| unsafe { w.bits(1 << 6) });
            let io_high = self.io_is_high();
            gpiof.bsrr.write(|w| unsafe { w.bits(1 << (6 + 16)) });
            if !io_high {
                break;
            }
            if deadline.wrapping_sub(Self::cyccnt()) >= 0x8000_0000 {
                return Err(SmartcardError::Timeout);
            }
        }

        let t0 = Self::cyccnt();

        Self::delay_until(t0.wrapping_add(etu / 2));

        let mut byte = 0u8;
        for bit_index in 0..8 {
            let sample_time = t0.wrapping_add(etu + (etu / 2) + (etu * bit_index as u32));
            Self::delay_until(sample_time);
            if self.io_is_high() {
                byte |= 1 << bit_index;
            }
        }

        let parity_time = t0.wrapping_add(etu * 9 + (etu / 2));
        Self::delay_until(parity_time);
        let parity_high = self.io_is_high();

        let total_ones = byte.count_ones() + if parity_high { 1 } else { 0 };
        // ISO 7816-3: EVEN parity — total 1s should be even. Warn if not.
        if !total_ones.is_multiple_of(2) {
            defmt::warn!(
                "RX parity error: byte=0x{:02X} parity={}",
                byte,
                parity_high
            );
        }

        Self::delay_until(t0.wrapping_add(etu * (11 + if self.protocol == 1 { 0 } else { 1 })));

        Ok(byte)
    }

    fn send_byte(&mut self, byte: u8) -> Result<(), SmartcardError> {
        let parity_is_one = (byte.count_ones() % 2) == 1;
        let etu = self.etu_cycles;
        let guard_etu: u32 = if self.protocol == 1 { 1 } else { 2 };
        let total_etu = 10 + guard_etu;

        self.io_drive_low();
        let t0 = Self::cyccnt();
        let low_ok = !self.io_is_high();
        Self::delay_until(t0.wrapping_add(etu));

        for bit_index in 0..8 {
            if (byte >> bit_index) & 1 != 0 {
                self.io_release_high();
            } else {
                self.io_drive_low();
            }
            Self::delay_until(t0.wrapping_add(etu * (1 + bit_index as u32 + 1)));
        }

        if parity_is_one {
            self.io_release_high();
        } else {
            self.io_drive_low();
        }
        Self::delay_until(t0.wrapping_add(etu * 10));

        self.io_release_high();
        let high_ok = self.io_is_high();
        Self::delay_until(t0.wrapping_add(etu * total_etu));

        if !low_ok || !high_ok {
            defmt::error!(
                "TX IO readback FAIL: byte=0x{:02X} low_ok={} high_ok={}",
                byte,
                low_ok,
                high_ok
            );
            diag_tlv(DTAG_TX_BYTE_ERR, &[byte, low_ok as u8, high_ok as u8]);
        }
        Ok(())
    }

    fn io_readback_test(&mut self) {
        self.io_release_high();
        Self::delay_cycles(self.etu_cycles);
        let high_ok = self.io_is_high();
        self.io_drive_low();
        let low_ok = !self.io_is_high();
        self.io_release_high();
        Self::delay_cycles(self.etu_cycles);
        defmt::info!("IO readback: high={} low={} (expect 1,1)", high_ok, low_ok);
        diag_tlv(DTAG_IO_READBACK, &[high_ok as u8, low_ok as u8]);
    }

    fn tx_single_byte_diagnostic(&mut self) {
        defmt::info!("TX diagnostic: sending 0xFF (PPSS) after ATR");
        cortex_m::interrupt::disable();
        // 4 ETU direction-change guard time
        Self::delay_cycles(self.etu_cycles * 4);
        let before_high = self.io_is_high();
        let _ = self.send_byte(0xFF);
        let after_high = self.io_is_high();
        defmt::info!(
            "TX diag: before_tx_high={} after_tx_high={}",
            before_high,
            after_high
        );
        defmt::info!("TX diag: waiting for card response (40ms)...");
        let result = match self.recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES) {
            Ok(b) => {
                defmt::info!("TX diag: card responded 0x{:02X}!", b);
                0u8
            }
            Err(SmartcardError::Timeout) => {
                defmt::warn!("TX diag: no card response (timeout)");
                1u8
            }
            Err(_) => {
                defmt::error!("TX diag: error");
                2u8
            }
        };
        unsafe {
            cortex_m::interrupt::enable();
        }
        diag_tlv(
            DTAG_TX_SINGLE,
            &[before_high as u8, after_high as u8, result],
        );
    }

    fn read_atr(&mut self) -> Result<(), SmartcardError> {
        let mut len = 0usize;

        loop {
            let timeout = if len == 0 {
                SC_ATR_TIMEOUT_CYCLES
            } else {
                SC_ATR_BYTE_TIMEOUT_CYCLES
            };

            match self.recv_byte_timeout(timeout) {
                Ok(b) => {
                    if len == 0 && b == 0x00 {
                        continue;
                    }
                    if len >= SC_ATR_MAX_LEN {
                        return Err(SmartcardError::BufferOverflow);
                    }
                    self.atr.raw[len] = b;
                    len += 1;
                }
                Err(SmartcardError::Timeout) if len > 0 => break,
                Err(e) => return Err(e),
            }
        }

        self.atr.len = len;
        if len == 0 {
            return Err(SmartcardError::InvalidATR);
        }
        Ok(())
    }

    fn detect_protocol_from_atr(&mut self) {
        if self.atr.len < 3 {
            self.protocol = 0;
            return;
        }
        let t0 = self.atr.raw[1];
        let y1 = (t0 >> 4) & 0x0F;
        let mut idx = 2;
        if y1 & 0x01 != 0 {
            idx += 1;
        }
        if y1 & 0x02 != 0 {
            idx += 1;
        }
        if y1 & 0x04 != 0 {
            idx += 1;
        }
        if y1 & 0x08 != 0 && idx < self.atr.len {
            let td1 = self.atr.raw[idx];
            self.protocol = td1 & 0x0F;
        }
    }

    fn negotiate_pps_fsm(&mut self, params: &AtrParams) -> Result<(), ()> {
        if !params.has_ta1 || params.ta1 == 0x11 {
            defmt::debug!("PPS: skipping (no TA1 or default Fi/Di)");
            return Ok(());
        }
        let mut fsm = PpsFsm::new();
        let req = fsm.build_request(params.protocol, params.ta1);
        defmt::info!("PPS: sending {} bytes", req.len());
        for &b in req.iter() {
            self.send_byte(b).map_err(|_| ())?;
        }
        fsm.start_response();
        loop {
            match self.recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES) {
                Ok(byte) => {
                    let state = fsm.process_byte(byte);
                    if state == PpsState::Done {
                        // PPS succeeded — switch to TA1's F/D parameters
                        // (matches F469 USART: set_baud_from_fi_di in smartcard.rs:403)
                        if params.di > 0 {
                            self.etu_cycles =
                                (SYSCLK_HZ / CARD_CLK_HZ) * params.fi as u32 / params.di as u32;
                        }
                        defmt::info!(
                            "PPS OK: Fi={} Di={} etu_cycles={}",
                            params.fi,
                            params.di,
                            self.etu_cycles
                        );
                        return Ok(());
                    }
                    if state == PpsState::Failed {
                        defmt::warn!("PPS failed: negotiation error");
                        return Err(());
                    }
                }
                Err(SmartcardError::Timeout) => {
                    fsm.set_timeout();
                    defmt::warn!("PPS failed: timeout, staying at D=1");
                    return Err(());
                }
                Err(_) => {
                    defmt::warn!("PPS failed: receive error");
                    return Err(());
                }
            }
        }
    }

    fn do_ifs_negotiation_t1(&mut self) -> Result<u8, ()> {
        const S_IFS_REQ: u8 = 0xC1;
        const S_IFS_RESP: u8 = 0xE1;
        const IFSD: u8 = 254;
        let lrc_val = 0u8 ^ S_IFS_REQ ^ 1u8 ^ IFSD;
        self.send_byte(0).map_err(|_| ())?;
        self.send_byte(S_IFS_REQ).map_err(|_| ())?;
        self.send_byte(1).map_err(|_| ())?;
        self.send_byte(IFSD).map_err(|_| ())?;
        self.send_byte(lrc_val).map_err(|_| ())?;
        let nad = self
            .recv_byte_timeout(SC_PROCEDURE_TIMEOUT_CYCLES)
            .map_err(|_| ())?;
        let pcb = self
            .recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES)
            .map_err(|_| ())?;
        let len = self
            .recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES)
            .map_err(|_| ())?;
        if (pcb & 0xC0) != 0xC0 || len != 1 {
            return Err(());
        }
        let ifsc = self
            .recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES)
            .map_err(|_| ())?;
        let lrc_recv = self
            .recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES)
            .map_err(|_| ())?;
        let lrc_exp = nad ^ pcb ^ len ^ ifsc;
        if lrc_recv != lrc_exp {
            return Err(());
        }
        if pcb == S_IFS_RESP {
            Ok(ifsc)
        } else {
            Err(())
        }
    }

    pub fn transmit_apdu(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> Result<usize, SmartcardError> {
        if !self.powered {
            return Err(SmartcardError::HardwareError);
        }

        cortex_m::interrupt::disable();
        self.io_release_high();
        Self::delay_cycles(self.etu_cycles * 4);

        let result = if self.protocol == 1 {
            let ifsc = self.ifsc;
            let mut ns = self.t1_ns;
            let r = transmit_apdu_t1(self, ifsc, &mut ns, command, response).map_err(|e| match e {
                T1Error::Transport(se) => se,
                T1Error::LrcMismatch | T1Error::Timeout => SmartcardError::ProtocolError,
            });
            self.t1_ns = ns;
            r
        } else {
            self.transmit_apdu_t0(command, response)
        };
        unsafe {
            cortex_m::interrupt::enable();
        }
        result
    }

    pub fn transmit_raw(
        &mut self,
        data: &[u8],
        response: &mut [u8],
    ) -> Result<usize, SmartcardError> {
        if !self.powered {
            return Err(SmartcardError::HardwareError);
        }

        cortex_m::interrupt::disable();
        let result = self.transmit_raw_inner(data, response);
        unsafe {
            cortex_m::interrupt::enable();
        }
        result
    }

    fn transmit_raw_inner(
        &mut self,
        data: &[u8],
        response: &mut [u8],
    ) -> Result<usize, SmartcardError> {
        for &byte in data {
            self.send_byte(byte)?;
        }
        self.io_release_high();
        Self::delay_cycles(self.etu_cycles * 22);
        let mut total_len = 0usize;
        let mut timeout_cycles = SC_PROCEDURE_TIMEOUT_CYCLES;
        while total_len < response.len() {
            match self.recv_byte_timeout(timeout_cycles) {
                Ok(byte) => {
                    response[total_len] = byte;
                    total_len += 1;
                    timeout_cycles = SC_BYTE_TIMEOUT_CYCLES;
                }
                Err(SmartcardError::Timeout) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(total_len)
    }

    fn transmit_apdu_t0(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> Result<usize, SmartcardError> {
        if command.len() < 5 {
            return Err(SmartcardError::ProtocolError);
        }
        let ins = command[1];
        let mut header = [command[0], command[1], command[2], command[3], command[4]];
        let mut data_offset = 5usize;
        let mut response_len = 0usize;
        let max_response = response.len();
        let mut get_response_count: u8 = 0;

        #[allow(clippy::never_loop)]
        loop {
            for &b in &header {
                self.send_byte(b)?;
            }

            // Process procedure bytes
            loop {
                let pb = self.wait_procedure_byte()?;

                // NULL — card needs more time
                if pb == 0x60 {
                    continue;
                }

                // ACK all remaining data
                if pb == ins {
                    // Send all remaining command body bytes
                    while data_offset < command.len() {
                        self.send_byte(command[data_offset])?;
                        data_offset += 1;
                    }
                    // Receive response data if header[4] indicates incoming
                    if header[1] == 0xC0 && header[4] > 0 {
                        let n = (header[4] as usize).min(max_response.saturating_sub(response_len));
                        for i in 0..n {
                            response[response_len + i] =
                                self.recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES)?;
                        }
                        response_len += n;
                    }
                    // After ACK + data transfer, card sends final SW1 SW2
                    let sw1 = self.recv_byte_timeout(SC_PROCEDURE_TIMEOUT_CYCLES)?;
                    let sw2 = self.recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES)?;
                    return Self::handle_sw12(
                        self,
                        sw1,
                        sw2,
                        response,
                        &mut response_len,
                        max_response,
                        &mut header,
                        &mut get_response_count,
                        data_offset,
                        command.len(),
                    );
                }

                // ACK one data byte
                if pb == (ins ^ 0xFF) {
                    if data_offset < command.len() {
                        self.send_byte(command[data_offset])?;
                        data_offset += 1;
                    }
                    continue;
                }

                // SW1 bytes — procedure byte IS the status word SW1
                let sw2 = self.recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES)?;
                return Self::handle_sw12(
                    self,
                    pb,
                    sw2,
                    response,
                    &mut response_len,
                    max_response,
                    &mut header,
                    &mut get_response_count,
                    data_offset,
                    command.len(),
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_sw12(
        &mut self,
        sw1: u8,
        sw2: u8,
        response: &mut [u8],
        response_len: &mut usize,
        max_response: usize,
        header: &mut [u8; 5],
        get_response_count: &mut u8,
        _data_offset: usize,
        _data_len: usize,
    ) -> Result<usize, SmartcardError> {
        if *response_len + 2 <= max_response {
            response[*response_len] = sw1;
            response[*response_len + 1] = sw2;
        }
        *response_len += 2;

        // 0x61 XX: response data available, need GET RESPONSE
        if sw1 == 0x61 && *get_response_count < SC_T0_GET_RESPONSE_MAX {
            *get_response_count += 1;
            *header = [0x00, 0xC0, 0x00, 0x00, sw2];
            // Continue in the 'send loop — but we need to return to 'send
            // Instead, inline the GET RESPONSE handling
            for &b in header.iter() {
                self.send_byte(b)?;
            }
            // Wait for procedure byte
            let pb = self.wait_procedure_byte()?;
            if pb == 0xC0 {
                // Receive response data
                let n = (sw2 as usize).min(max_response.saturating_sub(*response_len));
                for i in 0..n {
                    response[*response_len + i] = self.recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES)?;
                }
                *response_len += n;
                // Final SW1 SW2
                let fsw1 = self.recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES)?;
                let fsw2 = self.recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES)?;
                if *response_len + 2 <= max_response {
                    response[*response_len] = fsw1;
                    response[*response_len + 1] = fsw2;
                }
                *response_len += 2;
            } else if pb != 0x60 {
                // Got SW1 instead of procedure byte
                let fsw2 = self.recv_byte_timeout(SC_BYTE_TIMEOUT_CYCLES)?;
                if *response_len + 2 <= max_response {
                    response[*response_len] = pb;
                    response[*response_len + 1] = fsw2;
                }
                *response_len += 2;
            }
        }
        Ok(*response_len)
    }

    fn wait_procedure_byte(&mut self) -> Result<u8, SmartcardError> {
        let mut pb = self.recv_byte_timeout(SC_PROCEDURE_TIMEOUT_CYCLES)?;
        while pb == 0x60 {
            pb = self.recv_byte_timeout(SC_PROCEDURE_TIMEOUT_CYCLES)?;
        }
        Ok(pb)
    }
}

impl T1Transport for SmartcardBitbang {
    type Error = SmartcardError;

    fn send_byte(&mut self, b: u8) -> Result<(), Self::Error> {
        SmartcardBitbang::send_byte(self, b)
    }

    fn recv_byte_timeout(&mut self, ms: u32) -> Result<u8, Self::Error> {
        // Ensure at least 200ms timeout — CWT can be up to 820ms at default ETU.
        // The T=1 engine passes 20ms for inter-byte which is too short for
        // cards that process between bytes.
        let cycles = if ms < 200 {
            200u32.saturating_mul(SYSCLK_HZ / 1000)
        } else {
            ms.saturating_mul(SYSCLK_HZ / 1000)
        };
        SmartcardBitbang::recv_byte_timeout(self, cycles)
    }

    fn prepare_rx(&mut self) {}

    fn delay_bgt(&mut self) {
        Self::delay_cycles(self.etu_cycles * 22);
    }
}
