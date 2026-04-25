#![cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
#![allow(dead_code)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::needless_range_loop)]

use core::convert::Infallible;

use crate::ccid::SmartcardDriver;
use crate::pps_fsm::{di_from_ta1, fi_from_ta1, PpsFsm, PpsState};
use crate::t1_engine::{transmit_apdu_t1, T1Error, T1Transport};
use cortex_m::peripheral::DCB;
use stm32f7xx_hal::gpio::{
    gpiof::{PF10, PF6, PF7},
    gpioi::{PI0, PI2},
    Input, OpenDrain, Output, PushPull,
};
use stm32f7xx_hal::pac::{DWT, GPIOI, RCC, TIM10};

const SC_POWER_ON_DELAY_MS: u32 = 50;
const SC_RESET_DELAY_MS: u32 = 25;
const SC_ATR_POST_RST_DELAY_MS: u32 = 20;
const SC_CLK_TO_RST_DELAY_MS: u32 = 15;
const SC_ATR_TIMEOUT_MS: u32 = 400;
const SC_ATR_BYTE_TIMEOUT_MS: u32 = 1000;
const SC_BYTE_TIMEOUT_MS: u32 = 200;
const SC_PROCEDURE_TIMEOUT_MS: u32 = 5000;
const SC_T0_GET_RESPONSE_MAX: u8 = 32;
const SC_ATR_MAX_LEN: usize = 33;
const SC_DEFAULT_ETU: u32 = 372;
const SC_MAX_CLK_HZ: u32 = 5_000_000;
const TIM10_BRINGUP_ARR: u32 = 215;

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
    sysclk_hz: u32,
    etu_cycles: u32,
}

impl SmartcardBitbang {
    pub fn new(
        mut io_pin: PI0<Output<OpenDrain>>,
        clk_pin: PF6<Output<PushPull>>,
        mut rst_pin: PI2<Output<PushPull>>,
        pres_pin: PF10<Input>,
        mut pwr_pin: PF7<Output<PushPull>>,
        sysclk_hz: u32,
    ) -> Self {
        Self::enable_dwt();

        io_pin.set_high();
        rst_pin.set_low();
        pwr_pin.set_high();

        let etu_cycles = SC_DEFAULT_ETU * 61;

        let mut sc = Self {
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
            sysclk_hz,
            etu_cycles,
            clk_half_cycles: 30,
        };
        sc
    }

    fn enable_dwt() {
        unsafe {
            (*DCB::PTR).demcr.modify(|r| r | (1 << 24));
            DWT::unlock();
            (*DWT::PTR).cyccnt.write(0);
            (*DWT::PTR).ctrl.modify(|r| r | 1);
        }
    }

    fn get_cycle_count(&self) -> u32 {
        DWT::cycle_count()
    }

    fn delay_cycles(&self, cycles: u32) {
        let start = self.get_cycle_count();
        while self.get_cycle_count().wrapping_sub(start) < cycles {}
    }

    fn delay_etu(&self, etu_count: u32) {
        self.delay_cycles(self.etu_cycles.saturating_mul(etu_count));
    }

    fn delay_ms(ms: u32) {
        for _ in 0..ms {
            cortex_m::asm::delay(216_000);
        }
    }

    fn io_is_high(&self) -> bool {
        unsafe { ((*GPIOI::ptr()).idr.read().bits() & (1 << 0)) != 0 }
    }

    fn start_clock(&mut self) {
        self.clk_pin.set_low();
    }

    fn stop_clock(&mut self) {
        self.clk_pin.set_low();
    }

    fn tick_clock(&mut self) {
        self.clk_pin.set_high();
        cortex_m::asm::delay(self.clk_half_cycles);
        self.clk_pin.set_low();
        cortex_m::asm::delay(self.clk_half_cycles);
    }

    fn set_baud_from_fi_di(&mut self, fi: u16, di: u8) {
        if di == 0 {
            return;
        }
        self.etu_cycles =
            (((self.sysclk_hz as u64 * fi as u64) / (1_000_000u64 * di as u64)).max(1) as u32)
                .max(self.etu_cycles);
        defmt::info!("PPS: ETU={} cycles (Fi={}, Di={})", self.etu_cycles, fi, di);
    }

    pub fn is_card_present(&self) -> bool {
        self.pres_pin.is_high()
    }

    pub fn set_protocol(&mut self, protocol: u8) {
        self.protocol = protocol;
        defmt::info!("Protocol set to T={}", protocol);
    }

    pub fn set_clock(&mut self, enable: bool) {
        if enable {
            self.start_clock();
        } else {
            self.stop_clock();
        }
    }

    pub fn set_clock_and_rate(
        &mut self,
        _clock_hz: u32,
        rate_bps: u32,
    ) -> Result<(u32, u32), SmartcardError> {
        if rate_bps != 0 {
            self.etu_cycles = (self.sysclk_hz / rate_bps).max(1);
        }
        let actual_rate = (self.sysclk_hz / self.etu_cycles.max(1)).max(1);
        Ok((1_000_000, actual_rate))
    }

    fn do_ifs_negotiation_t1(&mut self) -> Result<u8, ()> {
        const S_IFS_REQ: u8 = 0xC1;
        const S_IFS_RESP: u8 = 0xE1;
        const IFSD: u8 = 254;

        let lrc_val = 0u8 ^ S_IFS_REQ ^ 1u8 ^ IFSD;
        defmt::info!("T=1 IFSD: sending S(IFS req) IFSD={}", IFSD);
        self.send_byte(0).map_err(|_| ())?;
        self.send_byte(S_IFS_REQ).map_err(|_| ())?;
        self.send_byte(1).map_err(|_| ())?;
        self.send_byte(IFSD).map_err(|_| ())?;
        self.send_byte(lrc_val).map_err(|_| ())?;

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

        loop {
            match self.receive_byte_timeout(200) {
                Ok(byte) => {
                    let state = fsm.process_byte(byte);
                    if state == PpsState::Done {
                        self.set_baud_from_fi_di(params.fi, params.di);
                        defmt::info!("PPS: success, Fi={} Di={}", params.fi, params.di);
                        return Ok(());
                    }
                    if state == PpsState::Failed {
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

    fn power_on_atr(&mut self) -> Result<&Atr, SmartcardError> {
        defmt::info!("PowerOn: card_present={}", self.is_card_present());
        if !self.is_card_present() {
            return Err(SmartcardError::NoCard);
        }

        self.pwr_pin.set_high();
        self.rst_pin.set_low();
        self.stop_clock();
        Self::delay_ms(200);
        self.atr = Atr::default();
        self.powered = false;
        self.protocol = 0;
        self.ifsc = 32;
        self.t1_ns = 0;
        self.io_pin.set_high();

        // ISO 7816-3 activation: VCC → CLK → RST
        self.pwr_pin.set_low();
        Self::delay_ms(SC_POWER_ON_DELAY_MS);
        let clk_iters = SC_CLK_TO_RST_DELAY_MS * 100;
        for _ in 0..clk_iters {
            self.tick_clock();
        }
        self.rst_pin.set_high();
        for _ in 0..(SC_ATR_POST_RST_DELAY_MS * 100) {
            self.tick_clock();
        }

        match self.read_atr() {
            Ok(()) => {
                self.powered = true;
                let atr_slice = &self.atr.raw[..self.atr.len];
                defmt::info!("ATR len={} hex={=[u8]:x}", self.atr.len, atr_slice);
                let params = parse_atr(atr_slice);
                self.detect_protocol_from_atr();

                let _ = self.negotiate_pps_fsm(&params);

                if self.protocol == 1 {
                    self.ifsc = params.ifsc;
                    defmt::info!("T=1: IFSC={}", self.ifsc);
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
        self.stop_clock();
        self.io_pin.set_high();
        self.powered = false;
        self.atr = Atr::default();
        self.protocol = 0;
        self.ifsc = 32;
        self.t1_ns = 0;
    }

    fn read_atr(&mut self) -> Result<(), SmartcardError> {
        let mut len = 0usize;

        loop {
            let timeout = if len == 0 {
                SC_ATR_TIMEOUT_MS
            } else {
                SC_ATR_BYTE_TIMEOUT_MS
            };

            match self.receive_byte_timeout(timeout) {
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
            defmt::error!("ATR: no bytes received");
            return Err(SmartcardError::InvalidATR);
        }

        defmt::info!("ATR: {} bytes received", len);
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
            defmt::info!(
                "Detected protocol T={} from TD1=0x{:02X}",
                self.protocol,
                td1
            );
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

    pub fn transmit_raw(
        &mut self,
        data: &[u8],
        response: &mut [u8],
    ) -> Result<usize, SmartcardError> {
        if !self.powered {
            return Err(SmartcardError::HardwareError);
        }

        defmt::info!("transmit_raw: TX {} bytes", data.len());
        for &byte in data {
            self.send_byte(byte)?;
        }

        let mut total_len = 0usize;
        let mut timeout_ms = 500u32;

        while total_len < response.len() {
            match self.receive_byte_timeout(timeout_ms) {
                Ok(byte) => {
                    response[total_len] = byte;
                    total_len += 1;
                    timeout_ms = 50;
                }
                Err(SmartcardError::Timeout) => break,
                Err(e) => return Err(e),
            }
        }

        defmt::info!("transmit_raw: RX {} bytes", total_len);
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
        let mut body_offset = 5usize;
        let mut response_len = 0usize;
        let max_response = response.len();
        let mut get_response_count: u8 = 0;

        'send: loop {
            for b in header {
                self.send_byte(b)?;
            }
            if body_offset < command.len() {
                for &b in &command[body_offset..] {
                    self.send_byte(b)?;
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
                        for b in [0x00u8, 0xC0, 0x00, 0x00, sw2] {
                            self.send_byte(b)?;
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

    pub fn send_byte(&mut self, data: u8) -> Result<(), SmartcardError> {
        let mut parity = 0u8;
        let mut d = data;
        for _ in 0..8 {
            parity ^= d & 1;
            d >>= 1;
        }

        self.io_pin.set_low();
        self.tick_clock();
        self.tick_clock();

        for i in 0..8 {
            if ((data >> i) & 1) != 0 {
                self.io_pin.set_high();
            } else {
                self.io_pin.set_low();
            }
            self.tick_clock();
            self.tick_clock();
        }

        if parity != 0 {
            self.io_pin.set_high();
        } else {
            self.io_pin.set_low();
        }
        self.tick_clock();
        self.tick_clock();

        self.io_pin.set_high();
        self.tick_clock();
        self.tick_clock();
        self.tick_clock();
        self.tick_clock();

        Ok(())
    }

    pub fn receive_byte_timeout(&mut self, timeout_ms: u32) -> Result<u8, SmartcardError> {
        let timeout_cycles = timeout_ms.saturating_mul(self.sysclk_hz / 1000);
        let start = self.get_cycle_count();

        while self.io_is_high() {
            self.tick_clock();
            if self.get_cycle_count().wrapping_sub(start) > timeout_cycles {
                return Err(SmartcardError::Timeout);
            }
        }

        cortex_m::interrupt::disable();

        let mut data = 0u8;
        for i in 0..8 {
            self.tick_clock();
            self.tick_clock();
            self.tick_clock();
            self.tick_clock();
            if self.io_is_high() {
                data |= 1 << i;
            }
            self.tick_clock();
            self.tick_clock();
            self.tick_clock();
            self.tick_clock();
        }

        self.tick_clock();
        self.tick_clock();
        self.tick_clock();
        self.tick_clock();
        self.tick_clock();
        self.tick_clock();
        self.tick_clock();
        self.tick_clock();

        unsafe {
            cortex_m::interrupt::enable();
        }

        Ok(data)
    }
}

impl T1Transport for SmartcardBitbang {
    type Error = SmartcardError;

    fn send_byte(&mut self, b: u8) -> Result<(), Self::Error> {
        SmartcardBitbang::send_byte(self, b)
    }

    fn recv_byte_timeout(&mut self, ms: u32) -> Result<u8, Self::Error> {
        self.receive_byte_timeout(ms)
    }

    fn prepare_rx(&mut self) {}
}

impl SmartcardDriver for SmartcardBitbang {
    type Error = SmartcardError;

    fn power_on(&mut self) -> Result<&[u8], Self::Error> {
        let atr = self.power_on_atr()?;
        Ok(&atr.raw[..atr.len])
    }

    fn power_off(&mut self) {
        SmartcardBitbang::power_off(self)
    }

    fn is_card_present(&self) -> bool {
        SmartcardBitbang::is_card_present(self)
    }

    fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Self::Error> {
        SmartcardBitbang::transmit_apdu(self, command, response)
    }

    fn transmit_raw(&mut self, data: &[u8], response: &mut [u8]) -> Result<usize, Self::Error> {
        SmartcardBitbang::transmit_raw(self, data, response)
    }

    fn set_protocol(&mut self, protocol: u8) {
        SmartcardBitbang::set_protocol(self, protocol)
    }

    fn set_clock(&mut self, enable: bool) {
        SmartcardBitbang::set_clock(self, enable)
    }

    fn set_clock_and_rate(
        &mut self,
        clock_hz: u32,
        rate_bps: u32,
    ) -> Result<(u32, u32), Self::Error> {
        SmartcardBitbang::set_clock_and_rate(self, clock_hz, rate_bps)
    }
}
