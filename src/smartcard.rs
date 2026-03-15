#![cfg(all(target_arch = "arm", target_os = "none"))]
#![allow(dead_code)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::needless_range_loop)]

use crate::pps_fsm::{di_from_ta1, fi_from_ta1, PpsFsm, PpsResult, PpsState};

use core::convert::Infallible;

use crate::t1_engine::{transmit_apdu_t1, T1Error, T1Transport};
use stm32f4xx_hal::gpio::{
    gpioa::{PA2, PA4},
    gpioc::{PC2, PC5},
    gpiog::PG10,
    Alternate, Input, OpenDrain, Output, PushPull,
};
use stm32f4xx_hal::pac::{RCC, USART2};
use stm32f4xx_hal::rcc::Clocks;

/// Power-on delay after asserting PWR (card supply stable). Increased for Seedkeeper (secondary experiment).
const SC_POWER_ON_DELAY_MS: u32 = 50;
/// Reset low duration; then release RST and wait before reading ATR.
const SC_RESET_DELAY_MS: u32 = 25;
/// Extra delay after RST high before first ATR byte (card startup).
const SC_ATR_POST_RST_DELAY_MS: u32 = 20;
/// Delay after CLK on before RST high (ISO 7816-3: min 40k clock cycles after CLK before RST high; ~11ms at 3.57MHz).
const SC_CLK_TO_RST_DELAY_MS: u32 = 15;
const SC_ATR_TIMEOUT_MS: u32 = 400;
/// Per-byte timeout during ATR; longer than SC_BYTE_TIMEOUT_MS (ISO 7816-3 initial character delay up to ~9600 ETU).
const SC_ATR_BYTE_TIMEOUT_MS: u32 = 1000;
const SC_BYTE_TIMEOUT_MS: u32 = 200;
const SC_PROCEDURE_TIMEOUT_MS: u32 = 5000;
const SC_T0_GET_RESPONSE_MAX: u8 = 32;
const SC_ATR_MAX_LEN: usize = 33;
const SC_DEFAULT_ETU: u32 = 372;
const SC_MAX_CLK_HZ: u32 = 5_000_000;

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

#[derive(Debug, Clone)]
pub struct Atr {
    pub raw: [u8; SC_ATR_MAX_LEN],
    pub len: usize,
}

impl Default for Atr {
    fn default() -> Self {
        Self {
            raw: [0u8; SC_ATR_MAX_LEN],
            len: 0,
        }
    }
}

/// Parsed ATR parameters (ISO 7816-3)
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

/// Parse ATR into AtrParams (ISO 7816-3 §8.2)
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
                // TA for T=1 (level ≥ 3, since TA2 is global "specific mode")
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

pub struct SmartcardUart {
    usart: USART2,
    _io_pin: PA2<Alternate<7, OpenDrain>>,
    _clk_pin: PA4<Alternate<7, PushPull>>,
    rst_pin: PG10<Output<PushPull>>,
    pres_pin: PC2<Input>,
    pwr_pin: PC5<Output<PushPull>>,
    atr: Atr,
    powered: bool,
    protocol: u8, // 0 = T=0, 1 = T=1
    ifsc: u8,     // T=1 card's IFSC (from IFS negotiation)
    t1_ns: u8,    // T=1 send sequence number (alternates 0/1 across APDUs)
    pclk1_hz: u32,
    card_clk_hz: u32,
}

impl SmartcardUart {
    pub fn new(
        usart: USART2,
        io_pin: PA2<Alternate<7, OpenDrain>>,
        clk_pin: PA4<Alternate<7, PushPull>>,
        rst_pin: PG10<Output<PushPull>>,
        pres_pin: PC2<Input>,
        pwr_pin: PC5<Output<PushPull>>,
        clocks: &Clocks,
    ) -> Self {
        let pclk1 = clocks.pclk1().raw();
        let mut sc = Self {
            usart,
            _io_pin: io_pin,
            _clk_pin: clk_pin,
            rst_pin,
            pres_pin,
            pwr_pin,
            atr: Atr::default(),
            powered: false,
            protocol: 0,
            ifsc: 32,
            t1_ns: 0,
            pclk1_hz: pclk1,
            card_clk_hz: 0,
        };
        sc.enable_usart2_clock();
        sc.init_usart(clocks);
        sc
    }

    fn enable_usart2_clock(&mut self) {
        unsafe {
            let rcc = &*RCC::ptr();
            rcc.apb1enr().modify(|_, w| w.usart2en().set_bit());
        }
        cortex_m::asm::delay(100);
        defmt::info!("USART2 clock enabled");
    }

    fn init_usart(&mut self, clocks: &Clocks) {
        let pclk1 = clocks.pclk1().raw();
        let prescaler = ((pclk1 + 2 * SC_MAX_CLK_HZ - 1) / (2 * SC_MAX_CLK_HZ))
            .min(31)
            .max(1) as u8;
        let card_clk = pclk1 / (2 * prescaler as u32);
        self.card_clk_hz = card_clk;
        let baudrate = (card_clk + SC_DEFAULT_ETU / 2) / SC_DEFAULT_ETU;
        let brr_val = pclk1 / baudrate;

        self.usart.brr().write(|w| unsafe {
            w.div_mantissa().bits((brr_val >> 4) as u16);
            w.div_fraction().bits((brr_val & 0x0F) as u8)
        });

        // Configure USART for smartcard mode.
        // CR1: UE=1, M=1(9bit), PCE=1(parity), TE=1, RE=1 = 0x340C
        self.usart.cr1().write(|w| unsafe { w.bits(0x340C) });
        // CR2: STOP=1.5, CLKEN=1, CPOL=0, CPHA=0, LBCL=1 = 0x3900
        self.usart.cr2().write(|w| unsafe { w.bits(0x3900) });
        // CR3: SCEN (bit 5), NACK disabled = 0x0020
        self.usart.cr3().write(|w| unsafe { w.bits(0x0020) });
        // GTPR: Guard time (upper byte), prescaler (lower byte)
        self.usart
            .gtpr()
            .write(|w| unsafe { w.bits((16u16 << 8) | prescaler as u16) });

        cortex_m::asm::delay(100);
        defmt::info!("USART2 init done (CR1=0x340C CR2=0x3900 CLKEN=1)");

        defmt::info!(
            "USART2: pclk1={} presc={} clk={} baud={}",
            pclk1,
            prescaler,
            card_clk,
            baudrate
        );
    }

    pub fn is_card_present(&self) -> bool {
        self.pres_pin.is_high()
    }

    /// Set protocol (0 = T=0, 1 = T=1)
    pub fn set_protocol(&mut self, protocol: u8) {
        self.protocol = protocol;
        defmt::info!("Protocol set to T={}", protocol);
    }

    /// Set ICC clock output (CCID IccClock). enable: true = restart, false = stop.
    /// STM32 USART smartcard mode: CR2.CLKEN (bit 11).
    pub fn set_clock(&mut self, enable: bool) {
        if enable {
            self.usart
                .cr2()
                .modify(|r, w| unsafe { w.bits(r.bits() | 0x0800) });
        } else {
            self.usart
                .cr2()
                .modify(|r, w| unsafe { w.bits(r.bits() & !0x0800) });
        }
    }

    /// Set data rate (and optionally clock) for CCID SetDataRateAndClockFrequency.
    /// Clock frequency is fixed by hardware; only baud rate (rate_bps) is applied.
    /// Returns (actual_clock_hz, actual_rate_bps). Clamps rate to valid BRR range.
    pub fn set_clock_and_rate(
        &mut self,
        _clock_hz: u32,
        rate_bps: u32,
    ) -> Result<(u32, u32), SmartcardError> {
        const MIN_BAUD: u32 = 9600;
        const MAX_BAUD: u32 = 5_000_000;
        let rate_bps = rate_bps.clamp(MIN_BAUD, MAX_BAUD);
        if rate_bps == 0 {
            return Err(SmartcardError::ProtocolError);
        }
        let brr_val = self.pclk1_hz / rate_bps;
        if brr_val < 16 {
            return Err(SmartcardError::ProtocolError); // would overrun
        }
        self.usart.brr().write(|w| unsafe {
            w.div_mantissa().bits((brr_val >> 4) as u16);
            w.div_fraction().bits((brr_val & 0x0F) as u8)
        });
        let actual_rate = self.pclk1_hz / brr_val;
        Ok((self.card_clk_hz, actual_rate))
    }

    fn set_baud_from_fi_di(&mut self, fi: u16, di: u8) {
        if di == 0 {
            return;
        }
        let fi = fi as u32;
        let di = di as u32;
        let baudrate = self.card_clk_hz * di / fi;
        let brr_val = self.pclk1_hz / baudrate;
        self.usart.brr().write(|w| unsafe {
            w.div_mantissa().bits((brr_val >> 4) as u16);
            w.div_fraction().bits((brr_val & 0x0F) as u8)
        });
        defmt::info!("PPS: baud updated to {} (Fi={}, Di={})", baudrate, fi, di);
    }

    /// T=1 IFSD negotiation: send S(IFS request) with IFSD=254, parse S(IFS response).
    fn do_ifs_negotiation_t1(&mut self) -> Result<u8, ()> {
        const S_IFS_REQ: u8 = 0xC1; // S(IFS request)
        const S_IFS_RESP: u8 = 0xE1; // S(IFS response) -- bit 5 set for response
        const IFSD: u8 = 254;
        let lrc_val = 0u8 ^ S_IFS_REQ ^ 1u8 ^ IFSD;
        defmt::info!("T=1 IFSD: sending S(IFS req) IFSD={}", IFSD);
        self.send_byte(0).map_err(|_| ())?; // NAD
        self.send_byte(S_IFS_REQ).map_err(|_| ())?; // PCB
        self.send_byte(1).map_err(|_| ())?; // LEN
        self.send_byte(IFSD).map_err(|_| ())?; // INF = IFSD value
        self.send_byte(lrc_val).map_err(|_| ())?; // LRC
                                                  // Drain TX echoes before receiving
        loop {
            let sr = self.usart.sr().read().bits();
            if (sr & ((1 << 5) | (1 << 3))) != 0 {
                let _ = self.usart.dr().read().dr().bits();
            } else {
                break;
            }
        }
        let nad = self.receive_byte_timeout(2000).map_err(|_| ())?;
        let pcb = self.receive_byte_timeout(500).map_err(|_| ())?;
        let len = self.receive_byte_timeout(500).map_err(|_| ())?;
        defmt::info!(
            "T=1 IFSD resp: NAD=0x{:02X} PCB=0x{:02X} LEN={}",
            nad,
            pcb,
            len
        );
        if (pcb & 0xC0) != 0xC0 || len != 1 {
            defmt::warn!("T=1 IFSD: unexpected PCB/LEN");
            return Err(());
        }
        let ifsc = self.receive_byte_timeout(500).map_err(|_| ())?;
        let lrc_recv = self.receive_byte_timeout(500).map_err(|_| ())?;
        let lrc_exp = nad ^ pcb ^ len ^ ifsc;
        if lrc_recv != lrc_exp {
            defmt::warn!(
                "T=1 IFSD: LRC mismatch recv=0x{:02X} exp=0x{:02X}",
                lrc_recv,
                lrc_exp
            );
            return Err(());
        }
        if pcb == S_IFS_RESP {
            defmt::info!("T=1 IFSD: card confirmed IFSC={}", ifsc);
            Ok(ifsc)
        } else {
            defmt::warn!("T=1 IFSD: unexpected response PCB=0x{:02X}", pcb);
            Err(())
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
        for &b in req {
            self.send_byte(b).map_err(|_| ())?;
        }

        fsm.start_response();

        let timeout_ms = 200u32;
        loop {
            match self.receive_byte_timeout(timeout_ms) {
                Ok(byte) => {
                    let state = fsm.process_byte(byte);
                    if state == PpsState::Done {
                        self.set_baud_from_fi_di(params.fi, params.di);
                        defmt::info!("PPS: success, Fi={} Di={}", params.fi, params.di);
                        return Ok(());
                    } else if state == PpsState::Failed {
                        defmt::warn!("PPS: negotiation failed");
                        return Err(());
                    }
                }
                Err(SmartcardError::Timeout) => {
                    fsm.set_timeout();
                    defmt::warn!("PPS: timeout - using default parameters");
                    return Err(());
                }
                Err(_) => {
                    defmt::warn!("PPS: receive error");
                    return Err(());
                }
            }
        }
    }

    pub fn power_on(&mut self) -> Result<&Atr, SmartcardError> {
        defmt::info!("PowerOn: card_present={}", self.is_card_present());
        if !self.is_card_present() {
            return Err(SmartcardError::NoCard);
        }

        // Full cold reset: power off, RST low, wait for card to fully discharge
        self.pwr_pin.set_high(); // VCC off
        self.rst_pin.set_low(); // RST asserted
        Self::delay_ms(200); // Long delay for card capacitor discharge
        self.atr = Atr::default();
        self.powered = false;

        // Clear any stale USART data/errors from previous session
        while self.usart.sr().read().rxne().bit_is_set() {
            let _ = self.usart.dr().read().dr().bits();
        }
        self.clear_usart_errors();

        // Activate: VCC on, wait, then CLK-to-RST delay, then RST high → card sends ATR
        // CLK is already running (CLKEN=1 from init, TE=1). ISO 7816-3: card must see
        // at least 40,000 clock cycles after CLK before RST is released (~11ms at 3.57 MHz).
        self.pwr_pin.set_low(); // VCC on
        Self::delay_ms(SC_POWER_ON_DELAY_MS); // 50ms VCC stabilize
        Self::delay_ms(SC_CLK_TO_RST_DELAY_MS); // 15ms: min 40k cycles after CLK before RST high
        self.rst_pin.set_high(); // Release RST → card starts ATR
        let cr1 = self.usart.cr1().read().bits();
        let cr2 = self.usart.cr2().read().bits();
        let cr3 = self.usart.cr3().read().bits();
        defmt::info!(
            "Activated: CR1=0x{:04X} CR2=0x{:04X} CR3=0x{:04X}",
            cr1,
            cr2,
            cr3
        );
        Self::delay_ms(2); // Brief settle before reading

        // NACK already disabled in init (CR3=0x0020); no action needed for ATR.

        match self.read_atr() {
            Ok(()) => {
                self.powered = true;
                // Log full ATR for debugging (protocol/voltage issues)
                let atr_slice = &self.atr.raw[..self.atr.len];
                defmt::info!("ATR len={} hex={=[u8]:x}", self.atr.len, atr_slice);
                let params = parse_atr(&self.atr.raw[..self.atr.len]);
                self.detect_protocol_from_atr();

                let _ = self.negotiate_pps_fsm(&params);

                if self.protocol == 0 {
                    // T=0: enable NACK (CR3 bit 4) for error signaling
                    self.usart
                        .cr3()
                        .modify(|r, w| unsafe { w.bits(r.bits() | (1 << 4)) });
                }
                if self.protocol == 1 {
                    self.ifsc = params.ifsc;
                    // T=1: switch STOP bits from 1.5 to 1.
                    // T=1 has 1 ETU guard time (no NACK), vs T=0's 2 ETU (with NACK).
                    // CR2 bits 13:12: 00 = 1 stop bit (11 was 1.5 stop bits)
                    self.usart
                        .cr2()
                        .modify(|r, w| unsafe { w.bits(r.bits() & !0x3000) });
                    // Reduce guard time for T=1 (GT=1 is minimum: 1 stop-bit ETU)
                    let psc = self.usart.gtpr().read().bits() & 0xFF;
                    self.usart
                        .gtpr()
                        .write(|w| unsafe { w.bits((1u16 << 8) | psc) });
                    defmt::info!(
                        "T=1: IFSC={}, STOP=1bit, GT=1, CR2=0x{:04X}",
                        self.ifsc,
                        self.usart.cr2().read().bits()
                    );
                    match self.do_ifs_negotiation_t1() {
                        Ok(ifsc) => {
                            self.ifsc = ifsc;
                            defmt::info!("T=1 IFSD OK: card IFSC={}", self.ifsc);
                        }
                        Err(()) => {
                            defmt::warn!(
                                "T=1 IFSD negotiation failed, using ATR IFSC={}",
                                self.ifsc
                            );
                        }
                    }
                }
                defmt::info!("ATR OK, len={}, protocol=T={}", self.atr.len, self.protocol);
                Ok(&self.atr)
            }
            Err(e) => {
                defmt::error!("ATR failed");
                Err(e)
            }
        }
    }

    pub fn power_off(&mut self) {
        self.rst_pin.set_low();
        self.pwr_pin.set_high();
        self.powered = false;
        self.atr = Atr::default();
        self.protocol = 0;
        self.ifsc = 32;
        self.t1_ns = 0;
    }

    /// Clear USART error flags (ORE/PE/FE). Reading DR clears ORE per STM32 ref manual.
    fn clear_usart_errors(&mut self) {
        let sr = self.usart.sr().read().bits();
        if (sr & (1 << 3)) != 0 {
            let _ = self.usart.dr().read().dr().bits();
            defmt::debug!("USART cleared ORE");
        }
    }

    fn read_atr(&mut self) -> Result<(), SmartcardError> {
        // Clear ORE if set (read SR then DR), but ONLY if ORE is set -- don't discard
        // a valid byte that might already be sitting in DR (e.g. TS).
        let sr = self.usart.sr().read().bits();
        if (sr & (1 << 3)) != 0 && (sr & (1 << 5)) == 0 {
            // ORE without RXNE: just clear the error
            let _ = self.usart.dr().read().dr().bits();
        }

        // Wait for first byte (TS) with long timeout.
        // Card may already have TS in DR if it started ATR during power_on.
        let mut countdown = SC_ATR_TIMEOUT_MS;
        while !self.usart.sr().read().rxne().bit_is_set() {
            Self::delay_ms(1);
            countdown -= 1;
            if countdown == 0 {
                let sr_val = self.usart.sr().read().bits();
                defmt::error!("ATR timeout SR=0x{:04X}", sr_val);
                return Err(SmartcardError::Timeout);
            }
        }

        // Tight busy-wait loop to read all ATR bytes.
        // At 168MHz, spinning is fine. NO defmt, NO delay_ms during reception.
        // The USART buffers only 1 byte; we must read DR before the next byte arrives (~1ms).
        let mut len = 0usize;
        // Timeout counter: ~50ms inter-byte timeout at ~168 cycles per inner iteration
        let timeout_reload: u32 = 50 * 168_000; // ~50ms worth of spin iterations
        let mut timeout_counter = timeout_reload;

        loop {
            let sr = self.usart.sr().read().bits();

            if (sr & (1 << 5)) != 0 || (sr & (1 << 3)) != 0 {
                // RXNE or ORE: read the byte (valid in both cases)
                let b = self.usart.dr().read().dr().bits() as u8;
                if len == 0 && b == 0x00 {
                    continue; // skip leading nulls
                }
                if len < SC_ATR_MAX_LEN {
                    self.atr.raw[len] = b;
                    len += 1;
                }
                timeout_counter = timeout_reload;
                continue;
            }

            // No byte ready — spin and count down
            timeout_counter -= 1;
            if timeout_counter == 0 {
                if len > 0 {
                    break; // inter-byte timeout, ATR complete
                } else {
                    // Shouldn't happen (we waited for RXNE above)
                    defmt::error!("ATR: no bytes in tight loop");
                    return Err(SmartcardError::InvalidATR);
                }
            }
        }

        self.atr.len = len;
        if len == 0 {
            defmt::error!("ATR: no bytes received");
            return Err(SmartcardError::InvalidATR);
        }
        let atr_slice = &self.atr.raw[..len];
        defmt::info!("ATR: {} bytes: {=[u8]:x}", len, atr_slice);
        Ok(())
    }

    /// Detect protocol from ATR TD1 byte
    fn detect_protocol_from_atr(&mut self) {
        // Parse ATR to find TD1
        // T0 byte format: Y1 (bits 8-5), K (bits 4-1)
        if self.atr.len < 3 {
            self.protocol = 0;
            return;
        }

        let t0 = self.atr.raw[1];
        let y1 = (t0 >> 4) & 0x0F;
        let mut idx = 2;

        // Skip interface bytes based on Y1
        if y1 & 0x01 != 0 {
            idx += 1;
        } // TA1
        if y1 & 0x02 != 0 {
            idx += 1;
        } // TB1
        if y1 & 0x04 != 0 {
            idx += 1;
        } // TC1
        if y1 & 0x08 != 0 {
            // TD1 present - contains protocol
            if idx < self.atr.len {
                let td1 = self.atr.raw[idx];
                self.protocol = td1 & 0x0F;
                defmt::info!(
                    "Detected protocol T={} from TD1=0x{:02X}",
                    self.protocol,
                    td1
                );
            }
        }
    }

    /// Transmit APDU using current protocol
    pub fn transmit_apdu(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> Result<usize, SmartcardError> {
        if !self.powered {
            return Err(SmartcardError::HardwareError);
        }
        if command.len() >= 2 {
            defmt::info!(
                "APDU T={} CLA=0x{:02X} INS=0x{:02X} len={}",
                self.protocol,
                command[0],
                command[1],
                command.len()
            );
        } else {
            defmt::warn!("APDU too short len={}", command.len());
        }
        if self.protocol == 1 {
            let ifsc = self.ifsc;
            let mut ns = self.t1_ns;
            let result = transmit_apdu_t1(self, ifsc, &mut ns, command, response).map_err(|e| {
                match &e {
                    T1Error::Transport(se) => defmt::warn!("T=1 Transport err={}", se),
                    T1Error::LrcMismatch => defmt::warn!("T=1 LrcMismatch"),
                    T1Error::Timeout => defmt::warn!("T=1 Timeout"),
                }
                match e {
                    T1Error::Transport(se) => se,
                    T1Error::LrcMismatch | T1Error::Timeout => SmartcardError::ProtocolError,
                }
            });
            self.t1_ns = ns;
            result
        } else {
            self.transmit_apdu_t0(command, response)
        }
    }

    /// Transmit raw bytes to the smartcard and receive raw response.
    /// Used for TPDU mode where the host sends complete T=1 blocks, and for PTS negotiation.
    /// The firmware acts as a transparent byte pipe using timeout-based read.
    pub fn transmit_raw(
        &mut self,
        data: &[u8],
        response: &mut [u8],
    ) -> Result<usize, SmartcardError> {
        if !self.powered {
            return Err(SmartcardError::HardwareError);
        }

        defmt::info!("transmit_raw: TX {} bytes", data.len());

        // Send all bytes from input data
        for &byte in data {
            self.send_byte(byte)?;
        }

        // Read response bytes with inter-byte timeout
        // Use 50ms inter-byte timeout, 500ms initial timeout
        // This works for both PTS negotiation (3 bytes) and T=1 blocks
        let mut total_len = 0;
        let mut timeout_ms = 500u32; // Initial wait for first byte

        while total_len < response.len() {
            match self.receive_byte_timeout(timeout_ms) {
                Ok(byte) => {
                    response[total_len] = byte;
                    total_len += 1;
                    timeout_ms = 50; // Shorter timeout for subsequent bytes
                }
                Err(SmartcardError::Timeout) => {
                    // No more bytes available
                    break;
                }
                Err(e) => return Err(e),
            }
        }

        defmt::info!("transmit_raw: RX {} bytes", total_len);
        Ok(total_len)
    }

    /// T=0 protocol APDU transmission (procedure bytes, GET RESPONSE 61 XX, wrong Le 6C XX)
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
        let mut body_offset = 5usize;
        let mut response_len = 0usize;
        let max_response = response.len();
        let mut get_response_count: u8 = 0;

        'send: loop {
            for i in 0..5 {
                self.send_byte(header[i])?;
            }
            if body_offset < command.len() {
                for i in body_offset..command.len() {
                    self.send_byte(command[i])?;
                }
                body_offset = command.len();
            }

            loop {
                let mut pb = self.receive_byte_timeout(SC_PROCEDURE_TIMEOUT_MS)?;
                while pb == 0x60 {
                    defmt::info!("T=0 NULL 0x60");
                    pb = self.receive_byte_timeout(SC_PROCEDURE_TIMEOUT_MS)?;
                }
                defmt::info!("T=0 procedure 0x{:02X}", pb);

                if pb == ins {
                    let sw1 = self.receive_byte_timeout(SC_PROCEDURE_TIMEOUT_MS)?;
                    let sw2 = self.receive_byte_timeout(SC_BYTE_TIMEOUT_MS)?;
                    if response_len + 2 <= max_response {
                        response[response_len] = sw1;
                        response[response_len + 1] = sw2;
                    }
                    response_len += 2;
                    if sw1 == 0x6C {
                        header[4] = sw2;
                        body_offset = 5;
                        continue 'send;
                    }
                    if sw1 == 0x61 && get_response_count < SC_T0_GET_RESPONSE_MAX {
                        get_response_count += 1;
                        for b in &[0x00u8, 0xC0, 0x00, 0x00, sw2] {
                            self.send_byte(*b)?;
                        }
                        pb = self.receive_byte_timeout(SC_PROCEDURE_TIMEOUT_MS)?;
                        while pb == 0x60 {
                            pb = self.receive_byte_timeout(SC_PROCEDURE_TIMEOUT_MS)?;
                        }
                        if pb == 0xC0 || pb == 0x4F {
                            let n = (sw2 as usize).min(max_response.saturating_sub(response_len));
                            for i in 0..n {
                                response[response_len + i] =
                                    self.receive_byte_timeout(SC_BYTE_TIMEOUT_MS)?;
                            }
                            response_len += n;
                            let sw1 = self.receive_byte_timeout(SC_BYTE_TIMEOUT_MS)?;
                            let sw2 = self.receive_byte_timeout(SC_BYTE_TIMEOUT_MS)?;
                            if response_len + 2 <= max_response {
                                response[response_len] = sw1;
                                response[response_len + 1] = sw2;
                            }
                            response_len += 2;
                            if sw1 == 0x61 {
                                header = [0x00, 0xC0, 0x00, 0x00, sw2];
                                body_offset = 5;
                                continue 'send;
                            }
                        }
                        return Ok(response_len);
                    }
                    return Ok(response_len);
                }
                if pb == (ins ^ 0xFF) {
                    if body_offset < command.len() {
                        self.send_byte(command[body_offset])?;
                        body_offset += 1;
                    }
                    continue;
                }
                if pb == 0x61 {
                    let sw2 = self.receive_byte_timeout(SC_BYTE_TIMEOUT_MS)?;
                    if get_response_count >= SC_T0_GET_RESPONSE_MAX {
                        if response_len + 2 <= max_response {
                            response[response_len] = 0x61;
                            response[response_len + 1] = sw2;
                        }
                        return Ok(response_len + 2);
                    }
                    get_response_count += 1;
                    header = [0x00, 0xC0, 0x00, 0x00, sw2];
                    body_offset = 5;
                    continue 'send;
                }
                if pb == 0x6C {
                    let sw2 = self.receive_byte_timeout(SC_BYTE_TIMEOUT_MS)?;
                    header[4] = sw2;
                    body_offset = 5;
                    continue 'send;
                }
                let sw2 = self.receive_byte_timeout(SC_BYTE_TIMEOUT_MS)?;
                if response_len + 2 <= max_response {
                    response[response_len] = pb;
                    response[response_len + 1] = sw2;
                }
                return Ok(response_len + 2);
            }
        }
    }

    pub fn receive_byte_timeout(&mut self, timeout_ms: u32) -> Result<u8, SmartcardError> {
        // Tight spin-wait for RXNE. At 168MHz, each iteration is ~6 cycles.
        // For 1ms: ~28000 iterations. For timeout_ms: iterations = timeout_ms * 28000.
        let iterations: u32 = timeout_ms.saturating_mul(28_000);
        let mut countdown = iterations;
        loop {
            let sr = self.usart.sr().read().bits();
            if (sr & (1 << 5)) != 0 || (sr & (1 << 3)) != 0 {
                // RXNE or ORE: read DR (valid byte in both cases)
                let byte = self.usart.dr().read().dr().bits() as u8;
                // Check for USART errors (PE=bit0, FE=bit1, NE=bit2, ORE=bit3)
                if (sr & 0x0F) != 0 {
                    defmt::warn!("USART err SR=0x{:04X} byte=0x{:02X}", sr, byte);
                }
                return Ok(byte);
            }
            countdown -= 1;
            if countdown == 0 {
                let sr_final = self.usart.sr().read().bits();
                defmt::warn!("Rx timeout {}ms SR=0x{:04X}", timeout_ms, sr_final);
                return Err(SmartcardError::Timeout);
            }
        }
    }

    pub fn send_byte(&mut self, data: u8) -> Result<(), SmartcardError> {
        // Disable receiver to prevent echo capture on half-duplex I/O line
        self.usart
            .cr1()
            .modify(|r, w| unsafe { w.bits(r.bits() & !(1 << 2)) });
        self.usart
            .dr()
            .write(|w| unsafe { w.dr().bits(data as u16) });
        let mut timeout_ms = SC_BYTE_TIMEOUT_MS;
        while !self.usart.sr().read().tc().bit_is_set() {
            Self::delay_ms(1);
            timeout_ms -= 1;
            if timeout_ms == 0 {
                defmt::error!("Tx timeout");
                // Re-enable receiver before returning
                self.usart
                    .cr1()
                    .modify(|r, w| unsafe { w.bits(r.bits() | (1 << 2)) });
                return Err(SmartcardError::Timeout);
            }
        }
        // Re-enable receiver
        self.usart
            .cr1()
            .modify(|r, w| unsafe { w.bits(r.bits() | (1 << 2)) });
        Ok(())
    }

    fn delay_ms(ms: u32) {
        for _ in 0..ms {
            cortex_m::asm::delay(168_000);
        }
    }
}

impl T1Transport for SmartcardUart {
    type Error = SmartcardError;
    fn send_byte(&mut self, b: u8) -> Result<(), Self::Error> {
        SmartcardUart::send_byte(self, b)
    }
    fn recv_byte_timeout(&mut self, ms: u32) -> Result<u8, Self::Error> {
        self.receive_byte_timeout(ms)
    }
    fn prepare_rx(&mut self) {
        // Drain all stale bytes (TX echoes on half-duplex smartcard I/O line).
        // In STM32 USART smartcard mode, the receiver samples the TX pin, so
        // every transmitted byte produces an echo in the receive data register.
        let mut drained = 0u32;
        loop {
            let sr = self.usart.sr().read().bits();
            if (sr & ((1 << 5) | (1 << 3))) != 0 {
                let _ = self.usart.dr().read().dr().bits();
                drained += 1;
            } else {
                break;
            }
        }
        if drained > 0 {
            defmt::debug!("prepare_rx: drained {} stale bytes", drained);
        }
    }
}
