#![cfg(all(target_arch = "arm", target_os = "none"))]
#![allow(dead_code)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::needless_range_loop)]

use crate::pps_fsm::{PpsFsm, PpsState};
use crate::smartcard_common::{
    detect_protocol_from_atr, do_ifs_negotiation_t1, parse_atr, t1_block_total_len,
    transmit_apdu_t0, verify_atr_tck, Atr, AtrParams, SmartcardConfig, SmartcardError, SmartcardIo,
    DEFAULT_TA1, SC_ATR_MAX_LEN,
};

use crate::t1_engine::T1Transport;
use ccid_firmware_rs::dwt_watchdog::DwtWatchdog;
use stm32f4xx_hal::gpio::{
    gpioa::{PA2, PA4},
    gpioc::{PC2, PC5},
    gpiog::PG10,
    Alternate, Input, OpenDrain, Output, PushPull,
};
use stm32f4xx_hal::pac::{RCC, USART2};
use stm32f4xx_hal::rcc::Clocks;

const SYSCLK_HZ: u32 = 168_000_000;

/// Opt-in: apply a PPS-negotiated Fi/Di to the wire after a PPS-ACK.
///
/// Bench regression (2026-10-10, later reverted on main): enabling this broke
/// the ComSign eID session through the Cherry ("Card is unresponsive") while
/// all host tests passed. Suspected reader/card baud desync: pcscd may follow
/// IccPowerOn with SetDataRateAndClockFrequency (rewriting BRR below the
/// negotiated rate while the card runs at the higher Di), and the IFS-based
/// fallback can only observe, not prove, which side switched. `false` keeps
/// the wire at the ATR-default Di forever — the proven round-1 behavior.
const PPS_APPLY_NEGOTIATED_BAUD: bool = false;

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
    /// True while the wire runs at a PPS-negotiated baud above the ATR
    /// default (only ever set with PPS_APPLY_NEGOTIATED_BAUD).
    pps_speedup_active: bool,
    /// Wire baud established by the last accepted PPS (see clamp in
    /// set_clock_and_rate).
    negotiated_baud: u32,
    /// One-shot: force the next PPS to the minimal (no PPS1) request —
    /// used by the IFS fallback ladder to re-activate at the default Di.
    pps_suppress_once: bool,
    pub(crate) config: SmartcardConfig,
}

impl SmartcardUart {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        usart: USART2,
        io_pin: PA2<Alternate<7, OpenDrain>>,
        clk_pin: PA4<Alternate<7, PushPull>>,
        rst_pin: PG10<Output<PushPull>>,
        pres_pin: PC2<Input>,
        pwr_pin: PC5<Output<PushPull>>,
        clocks: &Clocks,
        config: SmartcardConfig,
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
            pps_speedup_active: false,
            negotiated_baud: 0,
            pps_suppress_once: false,
            config,
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
        let prescaler = ((pclk1 + 2 * self.config.max_clk_hz - 1) / (2 * self.config.max_clk_hz))
            .min(31)
            .max(1) as u8;
        let card_clk = pclk1 / (2 * prescaler as u32);
        self.card_clk_hz = card_clk;
        let baudrate = (card_clk + self.config.default_etu / 2) / self.config.default_etu;
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
        // Experimental PPS path only: a host SetDataRateAndClockFrequency
        // below the PPS-negotiated rate would silently slow the reader side
        // while the card keeps running at the negotiated Di — the suspected
        // mechanism behind the f5feb61 bench regression. Hold the negotiated
        // rate and report the actual value back.
        let rate_bps = if self.pps_speedup_active {
            rate_bps.clamp(self.negotiated_baud.max(MIN_BAUD), MAX_BAUD)
        } else {
            rate_bps.clamp(MIN_BAUD, MAX_BAUD)
        };
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

    /// Apply a PPS-negotiated Fi/Di to the wire. Returns true when the baud
    /// actually changed (caller tracks this for the fallback ladder).
    fn set_baud_from_fi_di(&mut self, fi: u16, di: u8) -> bool {
        if di == 0 {
            return false;
        }
        let fi = fi as u32;
        let di = di as u32;
        let baudrate = self.card_clk_hz * di / fi;
        let brr_val = self.pclk1_hz / baudrate;
        if brr_val < 16 {
            // USART would overrun at this rate; keep the ATR-default baud.
            defmt::warn!(
                "PPS: baud {} needs BRR {} (<16) - keeping default baud",
                baudrate,
                brr_val
            );
            return false;
        }
        self.usart.brr().write(|w| unsafe {
            w.div_mantissa().bits((brr_val >> 4) as u16);
            w.div_fraction().bits((brr_val & 0x0F) as u8)
        });
        self.negotiated_baud = baudrate;
        defmt::info!("PPS: baud updated to {} (Fi={}, Di={})", baudrate, fi, di);
        true
    }

    /// Restore the ATR-default baud (Fi=372/Di=1), the rate every cold ATR
    /// must be read at. Idempotent when the wire is already at the default.
    fn reset_baud_default(&mut self) {
        let baudrate = (self.card_clk_hz + self.config.default_etu / 2) / self.config.default_etu;
        let brr_val = self.pclk1_hz / baudrate;
        self.usart.brr().write(|w| unsafe {
            w.div_mantissa().bits((brr_val >> 4) as u16);
            w.div_fraction().bits((brr_val & 0x0F) as u8)
        });
    }

    /// PPS negotiation (ISO 7816-3 §9). With PPS_APPLY_NEGOTIATED_BAUD the
    /// request carries PPS1 = the card's TA1 and an exact-echo ACK switches
    /// the wire to the offered Fi/Di. Any other response shape — mute card,
    /// PPS0 without the PPS1 bit, a PPS1 the card changed, checksum or echo
    /// mismatch — is rejected by the FSM and the wire silently stays at the
    /// ATR default: the session is never left desynchronized by PPS.
    fn negotiate_pps_fsm(&mut self, params: &AtrParams, suppress_speedup: bool) -> Result<(), ()> {
        if !params.has_ta1 || params.ta1 == DEFAULT_TA1 {
            defmt::debug!("PPS: skipping (no TA1 or default Fi/Di)");
            return Ok(());
        }

        let mut fsm = PpsFsm::new();
        let req = if PPS_APPLY_NEGOTIATED_BAUD && !suppress_speedup {
            fsm.build_request(params.protocol, params.ta1)
        } else {
            fsm.build_minimal_request(params.protocol)
        };

        if PPS_APPLY_NEGOTIATED_BAUD && !suppress_speedup {
            defmt::info!(
                "PPS: sending {} bytes (TA1=0x{:02X})",
                req.len(),
                params.ta1
            );
        } else {
            defmt::info!("PPS: sending {} bytes (minimal, no PPS1)", req.len());
        }
        for &b in req {
            self.send_byte(b).map_err(|_| ())?;
        }

        // Allow half-duplex line to settle after TX before enabling RX.
        // The card needs guard time + a few ETUs to turn around and respond.
        // At 11290 baud, 1 ETU ≈ 89μs; 2ms gives ~22 ETUs of margin.
        Self::delay_ms(2);

        // Drain any TX echo bytes and clear USART error flags.
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
        // Clear any residual FE/PE/NE flags by reading SR then DR if ORE set
        let sr = self.usart.sr().read().bits();
        if (sr & (1 << 3)) != 0 {
            let _ = self.usart.dr().read().dr().bits();
        }
        if drained > 0 {
            defmt::debug!("PPS: drained {} echo bytes", drained);
        }

        fsm.start_response();

        let timeout_ms = 200u32;
        loop {
            match self.receive_byte_timeout(timeout_ms) {
                Ok(byte) => {
                    let state = fsm.process_byte(byte);
                    if state == PpsState::Done {
                        if PPS_APPLY_NEGOTIATED_BAUD && !suppress_speedup {
                            // The card echoed our exact PPS1; it switches to
                            // the offered Fi/Di right after this response, so
                            // the reader may switch now. If applying is
                            // refused (BRR range) we stay at the default and
                            // so does the card (it never saw the refusal).
                            self.pps_speedup_active =
                                self.set_baud_from_fi_di(params.fi, params.di);
                        } else {
                            defmt::info!("PPS: success (minimal, protocol confirmed)");
                        }
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
        // Every cold ATR is sent at the ATR-default Di; a previous session
        // may have left the wire at a PPS-negotiated speed.
        self.pps_speedup_active = false;
        self.reset_baud_default();
        let pps_suppress = self.pps_suppress_once;
        self.pps_suppress_once = false;

        // Clear any stale USART data/errors from previous session
        while self.usart.sr().read().rxne().bit_is_set() {
            let _ = self.usart.dr().read().dr().bits();
        }
        self.clear_usart_errors();

        // Activate: VCC on, wait, then CLK-to-RST delay, then RST high → card sends ATR
        // CLK is already running (CLKEN=1 from init, TE=1). ISO 7816-3: card must see
        // at least 40,000 clock cycles after CLK before RST is released (~11ms at 3.57 MHz).
        self.pwr_pin.set_low(); // VCC on
        Self::delay_ms(self.config.power_on_delay_ms); // 50ms VCC stabilize
        Self::delay_ms(self.config.clk_to_rst_delay_ms); // 15ms: min 40k cycles after CLK before RST high
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
                self.protocol = detect_protocol_from_atr(&self.atr.raw[..self.atr.len]);
                defmt::info!("Detected protocol T={} from ATR", self.protocol);

                if !verify_atr_tck(&self.atr.raw[..self.atr.len], self.protocol) {
                    defmt::error!("ATR TCK mismatch for T=1, rejecting ATR");
                    self.powered = false;
                    return Err(SmartcardError::InvalidATR);
                }

                let _ = self.negotiate_pps_fsm(&params, pps_suppress);

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
                    match do_ifs_negotiation_t1(self) {
                        Ok(ifsc) => {
                            self.ifsc = ifsc;
                            defmt::info!("T=1 IFSD OK: card IFSC={}", self.ifsc);
                        }
                        Err(()) if self.pps_speedup_active => {
                            // Experimental PPS path only: a failed IFS at the
                            // negotiated baud means reader and card may
                            // disagree on the wire speed. Retry once at the
                            // same baud (transient), then at the ATR-default
                            // baud (card never switched), then cold
                            // re-activate with PPS suppressed — the session
                            // must never be left desynchronized.
                            defmt::warn!("T=1: IFS failed at PPS baud");
                            match do_ifs_negotiation_t1(self) {
                                Ok(ifsc) => {
                                    self.ifsc = ifsc;
                                    defmt::info!("T=1 IFSD OK on retry: card IFSC={}", self.ifsc);
                                }
                                Err(()) => {
                                    defmt::warn!("T=1: reverting to ATR-default baud");
                                    self.pps_speedup_active = false;
                                    self.reset_baud_default();
                                    match do_ifs_negotiation_t1(self) {
                                        Ok(ifsc) => {
                                            self.ifsc = ifsc;
                                            defmt::info!(
                                                "T=1 IFSD OK at default baud: IFSC={}",
                                                self.ifsc
                                            );
                                        }
                                        Err(()) => {
                                            defmt::warn!("T=1: cold re-activation at default baud");
                                            self.pps_suppress_once = true;
                                            self.power_off();
                                            if self.power_on().is_err() {
                                                self.pps_suppress_once = false;
                                                defmt::warn!(
                                                    "T=1 IFSD negotiation failed, using ATR IFSC={}",
                                                    self.ifsc
                                                );
                                            }
                                        }
                                    }
                                }
                            }
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
        // Restore USART to ATR convention for next cold reset:
        // 1.5 stop bits (CR2 STOP=11), 16 ETU guard time (GTPR GT=16),
        // ATR-default baud (a PPS session may have raised it).
        self.pps_speedup_active = false;
        self.reset_baud_default();
        self.usart
            .cr2()
            .modify(|r, w| unsafe { w.bits(r.bits() | 0x3000) });
        let psc = self.usart.gtpr().read().bits() & 0xFF;
        self.usart
            .gtpr()
            .write(|w| unsafe { w.bits((16u16 << 8) | psc) });
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
        let mut countdown = self.config.atr_timeout_ms;
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
        // Inter-byte timeout must be measured in real time (DWT cycle counter).
        // A raw iteration count is not wall-clock: one loop iteration is a
        // single volatile APB1 SR read plus branch (~15-25 cycles at 168 MHz),
        // so a 50*168_000-iteration counter spins for ~0.7-1 s — the dominant
        // part of the measured 1.5 s connect time.
        let mut wd = DwtWatchdog::from_ms(50, SYSCLK_HZ);
        wd.start();

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
                wd.start(); // reload inter-byte timeout
                continue;
            }

            // No byte ready — spin until the real-time deadline expires
            if wd.expired() {
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
            defmt::debug!(
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
            self.transmit_raw(command, response)
        } else {
            let proc_timeout = self.config.procedure_timeout_ms;
            let byte_timeout = self.config.byte_timeout_ms;
            transmit_apdu_t0(self, command, response, proc_timeout, byte_timeout)
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

        defmt::debug!("transmit_raw: TX {} bytes", data.len());

        // Cycle stamps (DWT) for bench decomposition: TX pacing, first
        // response byte (card turnaround), full exchange. debug!-level so
        // the default info build pays nothing; DEFMT_LOG=debug shows them.
        let mut stamp = DwtWatchdog::from_ms(60_000, SYSCLK_HZ);
        stamp.start();

        // Send all bytes from input data
        for &byte in data {
            self.send_byte(byte)?;
        }
        let tx_cyc = stamp.elapsed_cycles();

        let mut total_len = 0usize;

        // T=1: the card answers each exchange with exactly one block
        // (NAD PCB LEN INF... LRC, ISO 7816-3 §11.3). The block length is
        // known from the prologue, so the exchange terminates at the LRC
        // byte instead of waiting out a fixed inter-byte drain timeout
        // after the last byte (was a flat +50ms on every APDU).
        if self.protocol == 1 && response.len() >= 4 {
            let mut timeout_ms = 500u32; // first byte: card processing time
            let mut frx_cyc = 0u32;
            while total_len < 3 {
                match self.receive_byte_timeout(timeout_ms) {
                    Ok(byte) => {
                        response[total_len] = byte;
                        total_len += 1;
                        if total_len == 1 {
                            frx_cyc = stamp.elapsed_cycles();
                        }
                        timeout_ms = 50;
                    }
                    Err(SmartcardError::Timeout) => return Ok(total_len),
                    Err(e) => return Err(e),
                }
            }
            let expected =
                t1_block_total_len(&response[..3], response.len()).unwrap_or(response.len());
            while total_len < expected {
                match self.receive_byte_timeout(50) {
                    Ok(byte) => {
                        response[total_len] = byte;
                        total_len += 1;
                    }
                    Err(SmartcardError::Timeout) => break, // truncated block
                    Err(e) => return Err(e),
                }
            }
            defmt::debug!(
                "xfr: tx={}c frx={}c tot={}c rx={}",
                tx_cyc,
                frx_cyc,
                stamp.elapsed_cycles(),
                total_len
            );
            return Ok(total_len);
        }

        // Read response bytes with inter-byte timeout
        // Use 50ms inter-byte timeout, 500ms initial timeout
        // This works for both PTS negotiation (3 bytes) and T=1 blocks
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

        defmt::debug!("transmit_raw: RX {} bytes", total_len);
        Ok(total_len)
    }

    pub fn receive_byte_timeout(&mut self, timeout_ms: u32) -> Result<u8, SmartcardError> {
        let mut wd = DwtWatchdog::from_ms(timeout_ms, SYSCLK_HZ);
        wd.start();
        let mut safety_net: u32 = timeout_ms.saturating_mul(28_000).max(1_000_000);
        loop {
            let sr = self.usart.sr().read().bits();
            if (sr & (1 << 5)) != 0 || (sr & (1 << 3)) != 0 {
                let byte = self.usart.dr().read().dr().bits() as u8;
                if (sr & 0x0F) != 0 {
                    defmt::warn!("USART err SR=0x{:04X} byte=0x{:02X}", sr, byte);
                }
                return Ok(byte);
            }
            if wd.expired() {
                let sr_final = self.usart.sr().read().bits();
                defmt::warn!("Rx timeout {}ms SR=0x{:04X}", timeout_ms, sr_final);
                return Err(SmartcardError::Timeout);
            }
            safety_net -= 1;
            if safety_net == 0 {
                defmt::error!("Rx safety-net timeout {}ms", timeout_ms);
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
        // Tight TC poll bounded by a real-time DWT deadline. A delay_ms(1)
        // per-iteration poll quantizes every byte to whole milliseconds
        // (~1ms overshoot per byte at 11229 baud — ~40% of the wire time).
        let mut wd = DwtWatchdog::from_ms(self.config.byte_timeout_ms, SYSCLK_HZ);
        wd.start();
        while !self.usart.sr().read().tc().bit_is_set() {
            if wd.expired() {
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

impl SmartcardIo for SmartcardUart {
    fn send_byte(&mut self, byte: u8) -> Result<(), SmartcardError> {
        SmartcardUart::send_byte(self, byte)
    }
    fn recv_byte_timeout(&mut self, timeout_ms: u32) -> Result<u8, SmartcardError> {
        self.receive_byte_timeout(timeout_ms)
    }
    fn prepare_rx(&mut self) {
        // Drain TX echoes on half-duplex USART smartcard I/O
        loop {
            let sr = self.usart.sr().read().bits();
            if (sr & ((1 << 5) | (1 << 3))) != 0 {
                let _ = self.usart.dr().read().dr().bits();
            } else {
                break;
            }
        }
    }
}
