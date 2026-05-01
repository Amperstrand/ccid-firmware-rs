#![cfg(all(target_arch = "arm", target_os = "none"))]

use core::convert::Infallible;

use crate::pps_fsm::{di_from_ta1, fi_from_ta1};

pub const SC_ATR_MAX_LEN: usize = 33;
pub const SC_T0_GET_RESPONSE_MAX: u8 = 32;

// ISO 7816-3 T=0 procedure byte and SW1 constants
pub const SW1_NULL: u8 = 0x60; // NULL procedure byte — card needs more time
pub const SW1_GET_RESPONSE: u8 = 0x61; // Response data available (GET RESPONSE needed)
#[allow(dead_code)]
pub const SW1_WRONG_LENGTH: u8 = 0x6C; // Wrong Le — card proposes new Le in SW2
pub const INS_GET_RESPONSE: u8 = 0xC0; // GET RESPONSE instruction byte
pub const DEFAULT_TA1: u8 = 0x11; // Fi=372, Di=1 (default clock rate conversion)

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

pub struct Atr {
    pub raw: [u8; SC_ATR_MAX_LEN],
    pub len: usize,
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

pub fn detect_protocol_from_atr(atr: &[u8]) -> u8 {
    if atr.len() < 3 {
        return 0;
    }
    let t0 = atr[1];
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
    if y1 & 0x08 != 0 && idx < atr.len() {
        let td1 = atr[idx];
        return td1 & 0x0F;
    }
    0
}

/// Byte-level I/O for smartcard protocol functions.
/// Both USART and bitbang drivers implement this; timeouts are always in milliseconds.
pub trait SmartcardIo {
    fn send_byte(&mut self, byte: u8) -> Result<(), SmartcardError>;
    fn recv_byte_timeout(&mut self, timeout_ms: u32) -> Result<u8, SmartcardError>;
    /// Called after TX phase to drain stale data (e.g., USART echoes on half-duplex).
    /// Default: no-op (bitbang drivers don't need it).
    fn prepare_rx(&mut self) {}
}

/// Verify TCK (check byte) for T=1 ATRs per ISO 7816-3 §8.2.4.
/// TCK is the XOR of all bytes from T0 to the byte before TCK.
/// Returns true if verification passes (or TCK is not required for T=0).
pub fn verify_atr_tck(atr: &[u8], protocol: u8) -> bool {
    if protocol != 1 {
        return true;
    }
    if atr.len() < 3 {
        return true;
    }
    let expected: u8 = atr[1..atr.len() - 1].iter().fold(0u8, |acc, &b| acc ^ b);
    expected == atr[atr.len() - 1]
}

/// T=1 IFSD negotiation (ISO 7816-3 §11.4.2).
/// Send S(IFS request) with IFSD=254, parse S(IFS response) to get card's IFSC.
/// S-block format: NAD=0, PCB=0xC1/0xE1, LEN=1, INF=IFS value, LRC.
pub fn do_ifs_negotiation_t1(io: &mut dyn SmartcardIo) -> Result<u8, ()> {
    const S_IFS_REQ: u8 = 0xC1;
    const S_IFS_RESP: u8 = 0xE1;
    const IFSD: u8 = 254;
    let lrc_val = 0u8 ^ S_IFS_REQ ^ 1u8 ^ IFSD;
    io.send_byte(0).map_err(|_| ())?; // NAD
    io.send_byte(S_IFS_REQ).map_err(|_| ())?; // PCB
    io.send_byte(1).map_err(|_| ())?; // LEN
    io.send_byte(IFSD).map_err(|_| ())?; // INF
    io.send_byte(lrc_val).map_err(|_| ())?; // LRC

    io.prepare_rx(); // Drain TX echoes (no-op on bitbang)

    let nad = io.recv_byte_timeout(2000).map_err(|_| ())?;
    let pcb = io.recv_byte_timeout(500).map_err(|_| ())?;
    let len = io.recv_byte_timeout(500).map_err(|_| ())?;
    if (pcb & 0xC0) != 0xC0 || len != 1 {
        return Err(());
    }
    let ifsc = io.recv_byte_timeout(500).map_err(|_| ())?;
    let lrc_recv = io.recv_byte_timeout(500).map_err(|_| ())?;
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

/// T=0 protocol APDU transmission (procedure bytes, GET RESPONSE 61 XX, wrong Le 6C XX).
/// Generic over SmartcardIo so both USART and bitbang drivers share the same logic.
pub fn transmit_apdu_t0(
    io: &mut dyn SmartcardIo,
    command: &[u8],
    response: &mut [u8],
    procedure_timeout_ms: u32,
    byte_timeout_ms: u32,
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
            io.send_byte(header[i])?;
        }
        if body_offset < command.len() {
            for i in body_offset..command.len() {
                io.send_byte(command[i])?;
            }
            body_offset = command.len();
        }

        loop {
            let mut pb = io.recv_byte_timeout(procedure_timeout_ms)?;
            while pb == SW1_NULL {
                defmt::info!("T=0 NULL procedure byte");
                pb = io.recv_byte_timeout(procedure_timeout_ms)?;
            }
            defmt::info!("T=0 procedure 0x{:02X}", pb);

            if pb == ins {
                let sw1 = io.recv_byte_timeout(procedure_timeout_ms)?;
                let sw2 = io.recv_byte_timeout(byte_timeout_ms)?;
                if response_len + 2 <= max_response {
                    response[response_len] = sw1;
                    response[response_len + 1] = sw2;
                }
                response_len += 2;
                if sw1 == SW1_WRONG_LENGTH {
                    header[4] = sw2;
                    body_offset = 5;
                    continue 'send;
                }
                if sw1 == SW1_GET_RESPONSE && get_response_count < SC_T0_GET_RESPONSE_MAX {
                    get_response_count += 1;
                    for b in &[0x00u8, INS_GET_RESPONSE, 0x00, 0x00, sw2] {
                        io.send_byte(*b)?;
                    }
                    pb = io.recv_byte_timeout(procedure_timeout_ms)?;
                    while pb == SW1_NULL {
                        pb = io.recv_byte_timeout(procedure_timeout_ms)?;
                    }
                    if pb == INS_GET_RESPONSE || pb == 0x4F {
                        let le = if sw2 == 0 { 256usize } else { sw2 as usize };
                        let n = le.min(max_response.saturating_sub(response_len));
                        for i in 0..n {
                            response[response_len + i] = io.recv_byte_timeout(byte_timeout_ms)?;
                        }
                        response_len += n;
                        let sw1 = io.recv_byte_timeout(byte_timeout_ms)?;
                        let sw2 = io.recv_byte_timeout(byte_timeout_ms)?;
                        if response_len + 2 <= max_response {
                            response[response_len] = sw1;
                            response[response_len + 1] = sw2;
                        }
                        response_len += 2;
                        if sw1 == SW1_GET_RESPONSE {
                            header = [0x00, INS_GET_RESPONSE, 0x00, 0x00, sw2];
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
                    io.send_byte(command[body_offset])?;
                    body_offset += 1;
                }
                continue;
            }
            if pb == SW1_GET_RESPONSE {
                let sw2 = io.recv_byte_timeout(byte_timeout_ms)?;
                if get_response_count >= SC_T0_GET_RESPONSE_MAX {
                    if response_len + 2 <= max_response {
                        response[response_len] = SW1_GET_RESPONSE;
                        response[response_len + 1] = sw2;
                    }
                    return Ok(response_len + 2);
                }
                get_response_count += 1;
                header = [0x00, INS_GET_RESPONSE, 0x00, 0x00, sw2];
                body_offset = 5;
                continue 'send;
            }
            if pb == SW1_WRONG_LENGTH {
                let sw2 = io.recv_byte_timeout(byte_timeout_ms)?;
                header[4] = sw2;
                body_offset = 5;
                continue 'send;
            }
            let sw2 = io.recv_byte_timeout(byte_timeout_ms)?;
            if response_len + 2 <= max_response {
                response[response_len] = pb;
                response[response_len + 1] = sw2;
            }
            return Ok(response_len + 2);
        }
    }
}
