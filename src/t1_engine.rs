//! T=1 block protocol engine (ISO 7816-3)
//! I/R/S blocks, LRC, chaining for APDU > IFSC.
//! R-block from card: ACK (bits 1-0 = 00) or retransmit request (01 = EDC error, 10 = other);
//! on retransmit we resend the last I-block. S-block WTX supported.

use core::cmp::min;

/// R-block PCB: bits 1-0 = 00 ACK, 01 EDC error (resend), 10 other (resend) — ISO 7816-3
const R_BLOCK_MASK: u8 = 0x03;

#[derive(Debug)]
pub enum T1Error<E> {
    Transport(E),
    LrcMismatch,
    Timeout,
}

impl<E> From<E> for T1Error<E> {
    fn from(e: E) -> Self {
        T1Error::Transport(e)
    }
}

/// T=1 block types
const PCB_I_BLOCK: u8 = 0x00;
const PCB_R_BLOCK: u8 = 0x80;
const PCB_S_BLOCK: u8 = 0xC0;
const PCB_MASK: u8 = 0xC0;
const I_M_CHAIN: u8 = 0x20;

/// Max INF size (IFSC)
const T1_MAX_IFSC: usize = 254;
const T1_BLOCK_BUF: usize = 2 + 1 + T1_MAX_IFSC + 1;

/// Byte I/O for T=1 (implemented by smartcard layer)
pub trait T1Transport {
    type Error;
    fn send_byte(&mut self, b: u8) -> Result<(), Self::Error>;
    fn recv_byte_timeout(&mut self, ms: u32) -> Result<u8, Self::Error>;
    /// Called after TX phase is complete, before starting to receive.
    /// Implementations should drain any stale data (e.g., echoes on half-duplex USART).
    fn prepare_rx(&mut self) {}
}

fn lrc(buf: &[u8]) -> u8 {
    buf.iter().fold(0u8, |a, &b| a ^ b)
}

/// Send T=1 I-block (NAD=0, sequence 0, INF = apdu slice). Returns LRC.
fn send_i_block<T: T1Transport>(t: &mut T, inf: &[u8], ns: u8, m: bool) -> Result<(), T::Error> {
    let pcb = PCB_I_BLOCK | (ns << 6) | if m { I_M_CHAIN } else { 0 };
    let len = inf.len() as u8;
    defmt::info!(
        "T1 TX: [00 {:02x} {:02x}] + {} INF bytes + LRC",
        pcb,
        len,
        inf.len()
    );
    t.send_byte(0)?;
    t.send_byte(pcb)?;
    t.send_byte(len)?;
    for &b in inf {
        t.send_byte(b)?;
    }
    let lrc_val = 0u8 ^ 0 ^ pcb ^ len ^ lrc(inf);
    t.send_byte(lrc_val)?;
    defmt::info!(
        "T1 TX done: LRC=0x{:02X} ({} total bytes sent)",
        lrc_val,
        4 + inf.len()
    );
    Ok(())
}

/// Receive one T=1 block into buf [NAD, PCB, LEN, INF..., LRC]. Returns (pcb, inf_len).
/// Uses the first-byte timeout from `timeout_ms` for the initial NAD byte.
/// Subsequent bytes use a short inter-byte timeout via tight busy-wait.
fn recv_block<T: T1Transport>(
    t: &mut T,
    buf: &mut [u8],
    timeout_ms: u32,
) -> Result<(u8, usize), T1Error<T::Error>> {
    // Wait for first byte (NAD) with long timeout (card processing time).
    buf[0] = t
        .recv_byte_timeout(timeout_ms)
        .map_err(T1Error::Transport)?;
    // Remaining bytes must arrive quickly (within a few byte times).
    // Use short timeout to avoid overrun from 1ms polling delay.
    // 20ms is sufficient for 9600 baud (1 byte = 1.04ms) with margin.
    const INTER_BYTE_MS: u32 = 20;
    buf[1] = t
        .recv_byte_timeout(INTER_BYTE_MS)
        .map_err(T1Error::Transport)?;
    let len = t
        .recv_byte_timeout(INTER_BYTE_MS)
        .map_err(T1Error::Transport)? as usize;
    // NO defmt here -- any logging delay causes USART overrun on subsequent bytes.
    if len > buf.len().saturating_sub(4) {
        defmt::error!("T1 RX: LEN {} exceeds buffer", len);
        return Err(T1Error::LrcMismatch);
    }
    for i in 0..len {
        buf[3 + i] = t
            .recv_byte_timeout(INTER_BYTE_MS)
            .map_err(T1Error::Transport)?;
    }
    let lrc_recv = t
        .recv_byte_timeout(INTER_BYTE_MS)
        .map_err(T1Error::Transport)?;
    let lrc_exp = buf[0] ^ buf[1] ^ (len as u8) ^ lrc(&buf[3..3 + len]);
    // Now safe to log (all bytes received).
    // Hex dump the full block for debugging
    if len <= 20 {
        let end = 3 + len;
        defmt::info!(
            "T1 RX raw: [{=u8:02x} {=u8:02x} {=u8:02x}] INF={=[u8]:02x} LRC=0x{:02X}",
            buf[0],
            buf[1],
            len as u8,
            &buf[3..end],
            lrc_recv
        );
    } else {
        defmt::info!(
            "T1 RX raw: [{=u8:02x} {=u8:02x} {=u8:02x}] len={} LRC=0x{:02X}",
            buf[0],
            buf[1],
            len as u8,
            len,
            lrc_recv
        );
    }
    if lrc_recv != lrc_exp {
        defmt::error!(
            "T1 RX: LRC mismatch recv=0x{:02X} exp=0x{:02X}",
            lrc_recv,
            lrc_exp
        );
        return Err(T1Error::LrcMismatch);
    }
    defmt::info!("T1 RX: OK PCB=0x{:02X} LEN={}", buf[1], len);
    Ok((buf[1], len))
}

/// Transmit APDU over T=1. Single I-block or chained; collect response.
/// Handles R-block ACK (advance) and R-block retransmit request (resend last I-block).
/// `ns` is the send sequence number, persisted across calls by the caller.
pub fn transmit_apdu_t1<T: T1Transport>(
    t: &mut T,
    ifsc: u8,
    ns: &mut u8,
    apdu: &[u8],
    response: &mut [u8],
) -> Result<usize, T1Error<T::Error>> {
    const MAX_RETRANSMIT: u8 = 3;
    let ifsc = ifsc as usize;
    let mut offset = 0usize;
    let mut retransmit_count: u8 = 0;
    while offset < apdu.len() {
        let chunk_len = min(apdu.len() - offset, ifsc);
        let m = offset + chunk_len < apdu.len();
        send_i_block(t, &apdu[offset..offset + chunk_len], *ns, m).map_err(T1Error::Transport)?;
        if m {
            t.prepare_rx();
            let mut block = [0u8; T1_BLOCK_BUF];
            let (pcb, _) = recv_block(t, &mut block, 5000)?;
            if (pcb & PCB_MASK) == PCB_R_BLOCK {
                let r_type = pcb & R_BLOCK_MASK;
                if r_type != 0x00 {
                    // Card requests retransmit of block N(R)
                    retransmit_count += 1;
                    if retransmit_count > MAX_RETRANSMIT {
                        return Err(T1Error::LrcMismatch);
                    }
                    continue; // resend same chunk (offset unchanged)
                }
                retransmit_count = 0;
                *ns = (*ns + 1) & 1;
                offset += chunk_len;
                continue;
            }
            if (pcb & PCB_MASK) == PCB_S_BLOCK {
                // ISO 7816-3: WTX request = 0xC3 (type 3), WTX response = 0xCB
                if (pcb & 0x1F) == 0x03 {
                    let wtx = block.get(3).copied().unwrap_or(0);
                    let s_resp = [0u8, 0xCB, 1, wtx, 0u8]; // S(WTX response)
                    let l = 0 ^ s_resp[1] ^ s_resp[2] ^ s_resp[3];
                    t.send_byte(0).map_err(T1Error::Transport)?;
                    t.send_byte(s_resp[1]).map_err(T1Error::Transport)?;
                    t.send_byte(s_resp[2]).map_err(T1Error::Transport)?;
                    t.send_byte(s_resp[3]).map_err(T1Error::Transport)?;
                    t.send_byte(l).map_err(T1Error::Transport)?;
                }
                continue;
            }
        }
        offset += chunk_len;
        break;
    }

    let mut resp_len = 0usize;
    let mut first_rx = true;
    loop {
        if first_rx {
            t.prepare_rx();
            first_rx = false;
        }
        let mut block = [0u8; T1_BLOCK_BUF];
        let (pcb, inf_len) = recv_block(t, &mut block, 5000)?;
        if (pcb & 0x80) == 0 {
            // I-block: bit 7 = 0
            let n = min(inf_len, response.len().saturating_sub(resp_len));
            response[resp_len..resp_len + n].copy_from_slice(&block[3..3 + n]);
            resp_len += n;
            defmt::info!("T1 RX I-block: PCB=0x{:02X} inf_len={} copied={} total_resp_len={} M={}", pcb, inf_len, n, resp_len, (pcb & I_M_CHAIN) != 0);
            let m = (pcb & I_M_CHAIN) != 0;
            if !m {
                *ns = (*ns + 1) & 1;
                return Ok(resp_len);
            }
            // Extract card's N(S) from PCB bit 6, compute N(R) = (N(S) + 1) % 2
            // N(R) indicates the NEXT expected block number (ISO 7816-3)
            // If card sent N(S)=1, we send N(R)=0 meaning "I got block 1, send block 0 next"
            let card_ns = (pcb >> 6) & 1;
            let nr = (card_ns + 1) & 1;  // N(R) = (N(S) + 1) mod 2
            let r_pcb = PCB_R_BLOCK | (nr << 4);
            let r_lrc = 0u8 ^ r_pcb ^ 0u8;
            defmt::info!("T1 TX R-block: NAD=00 PCB=0x{:02X} LEN=00 LRC=0x{:02X} (card_ns={} nr={})", r_pcb, r_lrc, card_ns, nr);
            t.send_byte(0).map_err(T1Error::Transport)?;
            t.send_byte(r_pcb).map_err(T1Error::Transport)?;
            t.send_byte(0).map_err(T1Error::Transport)?;
            t.send_byte(r_lrc).map_err(T1Error::Transport)?;
            // Small delay to allow our R-block echoes to arrive before draining
            // In half-duplex smartcard mode, the card won't start transmitting
            // until after our transmission is complete
            cortex_m::asm::delay(10_000); // ~60us at 168MHz
            t.prepare_rx();
        } else if (pcb & PCB_MASK) == PCB_S_BLOCK {
            if (pcb & 0x1F) == 0x03 {
                let wtx = block.get(3).copied().unwrap_or(0);
                let l = 0 ^ 0xCB ^ 1 ^ wtx;
                t.send_byte(0).map_err(T1Error::Transport)?;
                t.send_byte(0xCB).map_err(T1Error::Transport)?;
                t.send_byte(1).map_err(T1Error::Transport)?;
                t.send_byte(wtx).map_err(T1Error::Transport)?;
                t.send_byte(l).map_err(T1Error::Transport)?;
            }
        }
    }
}
