//! #372 regression: sustained chained exchanges + the WTX-run pattern
//! through the pinned `iso14443` PCD session.
//!
//! Background (2026-10-21, issue #372): the dependency re-pin to the
//! rewritten session (73db6d4) introduced per-exchange work limits with
//! aggressive defaults (`max_consecutive_wtx: 8`, `max_total_wtx: 32`,
//! `max_retransmissions: 3`, `max_frames: 512`). A GlobalPlatform ISD
//! writing flash during a package LOAD legitimately emits long runs of
//! S(WTX) in the response phase of every APDU; at MFRC522-over-I2C
//! speeds that exceeds the default run budget, the session aborts with
//! `LimitExceeded(ConsecutiveWtx)`, and the reader surfaces it as the
//! SW 6400 seen on hardware (single APDUs stayed green because their
//! WTX runs are short). Our driver therefore configures explicit,
//! generous limits at session construction (`mfrc522_driver.rs`).
//!
//! Wire-level mock: responses are raw block bytes (PCB + INF + CRC_A)
//! so the test survives PCD-API churn and pins the WIRE CONTRACT —
//! chained-I-block INF integrity, R(ACK) echo-BN semantics, the
//! S(WTX) request/grant dance, and the budget behavior. PCB layout per
//! ISO/IEC 14443-4: I = 0x02|ns(+0x10 chaining), R(ACK) = 0xA2|nr,
//! S(WTX req) = 0xF2 wtxm; CRC_A is the reflected 0x8408 poly,
//! init 0x6363, low byte first. See docs/ISO14443_PCB_CONFORMANCE.md
//! for the standards analysis and both-options evidence.

#![cfg(all(test, feature = "backend-mfrc522"))]

use iso14443::type_a::limits::Limits;
use iso14443::type_a::vec::FrameVec;
use iso14443::type_a::{Ats, Frame, PcdSession, PcdTransceiver};
use std::vec::Vec;

#[derive(Debug)]
struct MockError;

/// Card-side state machine at the block level: ACKs a long PCD-side
/// I-block chain (echoing the block number), then — the flash-write
/// pattern — emits `wtx_run` consecutive S(WTX) requests in the
/// response phase before answering with its chained response (fixed
/// 20-byte payload, 8-byte chunks).
struct LoadClassCard {
    wtx_run: usize,
    grants_seen: usize,
    received: Vec<u8>,
    pcd_pcbs: Vec<u8>,
    pending: Vec<Vec<u8>>,
    last_final_bn: u8,
}

impl LoadClassCard {
    fn new(wtx_run: usize) -> Self {
        Self {
            wtx_run,
            grants_seen: 0,
            received: Vec::new(),
            pcd_pcbs: Vec::new(),
            pending: Vec::new(),
            last_final_bn: 0,
        }
    }
}

fn crc_a(data: &[u8]) -> [u8; 2] {
    // Mirrors the crate's own crc_a (its self-test: [0,0] -> A0 1E).
    let mut crc: u16 = 0x6363;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
        }
    }
    [(crc & 0xFF) as u8, (crc >> 8) as u8]
}

fn framed(block: &[u8]) -> FrameVec {
    let mut v = FrameVec::new();
    v.extend_from_slice(block);
    let c = crc_a(block);
    v.extend_from_slice(&c);
    v
}

impl PcdTransceiver for LoadClassCard {
    type Error = MockError;

    fn transceive(&mut self, frame: &Frame) -> Result<FrameVec, MockError> {
        let data = frame.data();
        let pcb = data[0];
        self.pcd_pcbs.push(pcb);
        // Software-CRC mode: strip the trailing CRC from sent frames.
        let body = if data.len() >= 2 {
            &data[..data.len() - 2]
        } else {
            data
        };
        let iblock = (pcb & 0x02) != 0 && (pcb & 0xC0) == 0;
        if iblock {
            self.received.extend_from_slice(&body[1..]);
            if (pcb & 0x10) != 0 {
                // Chained: R(ACK) echoing the received block number.
                let rack = 0xA2 | (pcb & 1);
                Ok(framed(&[rack]))
            } else {
                self.last_final_bn = pcb & 1;
                if self.wtx_run > 0 {
                    Ok(framed(&[0xF2, 0x02])) // S(WTX req, wtxm=2)
                } else {
                    self.serve_response()
                }
            }
        } else if (pcb & 0xC0) == 0xC0 && (pcb & 0x30) == 0x30 {
            // S(WTX response) = grant: keep asking until the run is spent.
            self.grants_seen += 1;
            if self.grants_seen < self.wtx_run {
                Ok(framed(&[0xF2, 0x02]))
            } else {
                self.wtx_run = 0;
                self.serve_response()
            }
        } else if let Some(next) = self.pending.first() {
            // R(ACK) of our chained response: serve the next chunk.
            let next = next.clone();
            self.pending.remove(0);
            Ok(framed(&next))
        } else {
            Err(MockError)
        }
    }

    fn try_enable_hw_crc(&mut self) -> Result<(), MockError> {
        Err(MockError) // force the software-CRC path
    }
}

impl LoadClassCard {
    /// Chained response (8-byte chunks); the first I-block carries the
    /// final PCD block's number, then toggles per chunk.
    fn serve_response(&mut self) -> Result<FrameVec, MockError> {
        let response: Vec<u8> = (0..20u8).collect();
        let chunks: Vec<&[u8]> = response.chunks(8).collect();
        let mut out = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let last = i + 1 == chunks.len();
            let ns = (self.last_final_bn + i as u8) & 1;
            let pcb_r = 0x02 | ns | if last { 0 } else { 0x10 };
            let mut b = Vec::with_capacity(1 + chunk.len());
            b.push(pcb_r);
            b.extend_from_slice(chunk);
            out.push(b);
        }
        let first = out.remove(0);
        self.pending = out;
        Ok(framed(&first))
    }
}

fn proven_ats() -> Ats {
    // The upstream fixture shape: [05 78 80 40 02] + CRC_A (07 F0).
    Ats::try_from([0x05u8, 0x78, 0x80, 0x40, 0x02, 0x07, 0xF0].as_slice()).expect("ats parse")
}

/// The limits our driver configures (must match mfrc522_driver.rs).
fn driver_limits() -> Limits {
    Limits {
        max_consecutive_wtx: 64,
        max_total_wtx: 4096,
        max_frames: 4096,
        max_retransmissions: 5,
        ..Limits::default()
    }
}

fn expected_response() -> Vec<u8> {
    (0..20u8).collect()
}

#[test]
fn sustained_load_with_wtx_run_survives_configured_limits() {
    // 460-byte APDU ≈ a gp LOAD block storm at FSC 32; 20 consecutive
    // WTX in the response phase ≈ a flash-writing ISD. With the limits
    // our driver configures, the exchange must complete intact.
    let apdu: Vec<u8> = (0..460u16).map(|i| (i % 251) as u8).collect();

    let mut card = LoadClassCard::new(20);
    let mut session = PcdSession::from_ats(&proven_ats(), None, false);
    session.set_fsc(32);
    session.set_limits(driver_limits());

    let got = session
        .exchange(&mut card, &apdu)
        .expect("sustained exchange with WTX run must complete");

    assert_eq!(
        card.received, apdu,
        "#372 class: PCD chaining must deliver every INF byte exactly once, in order"
    );
    assert_eq!(
        got.as_slice(),
        expected_response().as_slice(),
        "PICC chained response must reassemble in order"
    );
    assert_eq!(card.grants_seen, 20, "the full WTX dance must be granted");
    let chained = card
        .pcd_pcbs
        .iter()
        .filter(|&&p| (p & 0x02) != 0 && (p & 0xC0) == 0 && (p & 0x10) != 0)
        .count();
    assert!(chained >= 15, "expected a LOAD-class chain, saw {chained}");
}

#[test]
fn default_limits_abort_the_flash_write_pattern() {
    // Pins the upstream budget semantics: the DEFAULT limits abort a
    // 20-run WTX response. If this fails after a pin move, the defaults
    // changed and mfrc522_driver.rs limits may need retuning — see
    // docs/ISO14443_PCB_CONFORMANCE.md (#372).
    let apdu: Vec<u8> = (0..460u16).map(|i| (i % 251) as u8).collect();
    let mut card = LoadClassCard::new(20);
    let mut session = PcdSession::from_ats(&proven_ats(), None, false);
    session.set_fsc(32);
    let r = session.exchange(&mut card, &apdu);
    assert!(
        r.is_err(),
        "default limits must abort a 20-run WTX (got {r:?})"
    );
}

#[test]
fn single_block_exchange_baseline() {
    // Sanity: the same mock path handles the trivial exchange — keeps
    // the sustained test's failures unambiguous.
    let mut card = LoadClassCard::new(0);
    let mut session = PcdSession::from_ats(&proven_ats(), None, false);
    let got = session
        .exchange(&mut card, &[0x00, 0xA4])
        .expect("exchange");
    assert_eq!(got.as_slice(), expected_response().as_slice());
    assert_eq!(card.received, vec![0x00, 0xA4]);
}
