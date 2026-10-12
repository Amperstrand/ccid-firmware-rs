//! ISO 7816-3 §11 T=1 block endpoint — the reader terminates the host's
//! T=1 protocol and relays plain APDUs to the card (NFC ISO-DEP).
//!
//! WHY THIS EXISTS (the libccidtwin T=1 wall, root-caused 2026-10-09):
//! libccid's serial (GemPC Twin) backend has no CCID descriptor
//! negotiation — it hardcodes the host as T=1 initiator and exchanges
//! raw T=1 blocks through XfrBlock. A reader that merely relays those
//! bytes to the card gets SW=6E00 for every S-block (`00 C1 01 FE` IFS
//! request answered by the card as a garbage APDU). This module makes
//! the FIRMWARE the T=1 responder instead: S-blocks are answered
//! locally, I-block INF fields are assembled into APDUs, card responses
//! are re-framed as I-block chains.
//!
//! Block layout (§11.3): NAD(1) | PCB(1) | LEN(1) | INF(LEN) | LRC(1).
//! LRC = XOR of all preceding bytes (§11.3.3.1). The endpoint is the
//! T=1 "card" side; the host (libccid t1.c) is the initiator.
//!
//! PCBs (§11.4.2):
//! - I-block: 0 N(S) 0 M N(R)          — 0x00 | ns<<6 | m<<5 | nr
//! - R-block: 10 N(R) 0 ss             — 0x80 | nr<<3 | err
//! - S-block request/response: C0/D0 RESYNC, C1/D1 IFS, C2/D2 ABORT,
//!   C3/D3 WTX (response = request | 0x10).
//!
//! SPEC-CITED INVARIANTS:
//! - §11.6.2: S(IFS request N) announces the SENDER's new receive size;
//!   the peer replies S(IFS response N). libccid always opens with
//!   `00 C1 01 FE` — answered D1 FE, our send limit becomes 254.
//! - §11.6.3: chaining — all blocks of one chain share N(S); M=1 means
//!   more to follow; the receiver ACKs chain pieces with R(N(R)).
//! - §11.6.2 WTX: the responder needing more than BWT sends
//!   S(WTX request m); the initiator grants S(WTX response m) and waits
//!   m × BWT. Without this, card ops slower than the host's BWT
//!   (BWI=4 → ~177 ms; on-card RSA runs seconds) would time out.

use heapless::Vec;

/// Max INF field (§11.6.2 IFS ceiling is 254).
pub const MAX_IFSC: usize = 254;
/// Relay capacity: max APDU/response assembled from a chain. Public so
/// the CCID handler's copy buffer derives from it — independent sizes
/// caused the G-03 silent-truncation bug.
pub const MAX_RELAY: usize = 512;

const DEFAULT_HOST_IFSD: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pcb {
    /// N(S), more-chaining bit, N(R)
    I { ns: u8, more: bool, nr: u8 },
    /// N(R), error code 0..3 (0 = retransmit-request/ACK semantics)
    R { nr: u8, err: u8 },
    /// S-block; kind tag and whether request or response
    S { kind: SKind, response: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SKind {
    Resync,
    Ifs,
    Abort,
    Wtx,
}

fn parse_pcb(pcb: u8) -> Option<Pcb> {
    // ISO 7816-3 Table 45: N(S) at b7, M at b6, N(R) at b4 — in BOTH I-
    // and R-blocks. The b3..b1 positions are 0 (I) / error code (R).
    match pcb & 0x80 {
        0x00 => Some(Pcb::I {
            ns: (pcb >> 6) & 1,
            more: pcb & 0x20 != 0,
            nr: (pcb >> 4) & 1,
        }),
        0x80 => match pcb & 0xC0 {
            0x80 => Some(Pcb::R {
                nr: (pcb >> 4) & 1,
                err: pcb & 0x03,
            }),
            _ => {
                let response = pcb & 0x20 != 0;
                let kind = match pcb & 0x03 {
                    0x00 => SKind::Resync,
                    0x01 => SKind::Ifs,
                    0x02 => SKind::Abort,
                    0x03 => SKind::Wtx,
                    _ => return None,
                };
                Some(Pcb::S { kind, response })
            }
        },
        _ => None,
    }
}

fn lrc(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |a, b| a ^ b)
}

fn block(pcb: u8, inf: &[u8]) -> Vec<u8, 520> {
    let mut v = Vec::new();
    let _ = v.extend_from_slice(&[0x00, pcb, inf.len() as u8]);
    let _ = v.extend_from_slice(inf);
    let checksum = lrc(&v);
    let _ = v.push(checksum);
    v
}

fn i_pcb(ns: u8, more: bool, nr: u8) -> u8 {
    (ns << 6) | ((more as u8) << 5) | (nr << 4)
}

fn r_pcb(nr: u8, err: u8) -> u8 {
    0x80 | (nr << 4) | err
}

fn s_pcb(kind: SKind, response: bool) -> u8 {
    let base = match kind {
        SKind::Resync => 0xC0,
        SKind::Ifs => 0xC1,
        SKind::Abort => 0xC2,
        SKind::Wtx => 0xC3,
    };
    // §11.4.2 / proto-t1.c:38: S-response sets bit 5 (0x20), NOT bit 4.
    // The host engine validates `pcb == S | 0x20 | type` (proto-t1.c:806)
    // and rejects 0x1x-encodings.
    base | ((response as u8) << 5)
}

#[derive(Debug, PartialEq, Eq)]
pub enum FeedResult {
    /// Bytes to return to the host immediately (no card I/O).
    Immediate(Vec<u8, 520>),
    /// A complete APDU assembled from (possibly chained) I-blocks:
    /// relay it to the card, then call [`T1Endpoint::card_response`].
    Apdu(Vec<u8, MAX_RELAY>),
}

pub struct T1Endpoint {
    /// Our send sequence number (0/1), toggles per completed chain.
    ns: u8,
    /// Host send sequence we expect next (0/1), toggles per complete
    /// host chain.
    host_ns: u8,
    /// Max INF we send (§11.6.2 — raised by the host's IFS negotiation).
    send_ifsc: usize,
    /// Max INF we accept from the host.
    host_ifsd: usize,
    /// Incoming chain accumulator (host → card).
    recv_chain: Vec<u8, MAX_RELAY>,
    /// Outgoing chain remainder (card → host) after the first block.
    send_chain: heapless::Vec<u8, MAX_RELAY>,
    /// Last block we sent (for R-block retransmit, §11.5.3.3).
    last_sent: Option<Vec<u8, 520>>,
    /// Card response held while the WTX dance runs.
    wtx_pending: bool,
}

impl Default for T1Endpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl T1Endpoint {
    pub fn new() -> Self {
        T1Endpoint {
            ns: 0,
            host_ns: 0,
            send_ifsc: 32,
            host_ifsd: DEFAULT_HOST_IFSD,
            recv_chain: Vec::new(),
            send_chain: Vec::new(),
            last_sent: None,
            wtx_pending: false,
        }
    }

    pub fn reset(&mut self) {
        *self = T1Endpoint::new();
    }

    /// Process one host block (an XfrBlock payload while T=1 is active).
    pub fn feed(&mut self, bytes: &[u8]) -> FeedResult {
        if bytes.len() < 4 {
            return self.bad_block();
        }
        let (nad, pcb, len) = (bytes[0], bytes[1], bytes[2] as usize);
        let inf_end = 3 + len;
        if bytes.len() != inf_end + 1 || len > MAX_IFSC {
            return self.bad_block();
        }
        if lrc(&bytes[..inf_end]) != bytes[inf_end] {
            return self.bad_block();
        }
        let inf = &bytes[3..inf_end];

        match parse_pcb(pcb) {
            Some(Pcb::S { kind, response }) => {
                if response {
                    self.on_s_response(kind, inf)
                } else {
                    self.on_s_request(kind, inf)
                }
            }
            Some(Pcb::R { .. }) => {
                // Any R-block: retransmit the last block (§11.5.3.3).
                // Our chains are only ever ACKed mid-flight, so treating
                // every R as "send next/repeat" is the interoperable move.
                if self.wtx_pending {
                    // A WTX response arrives as an S-block, not R — but a
                    // stray R here means the host never saw the WTX: resend it.
                    if let Some(last) = &self.last_sent {
                        return FeedResult::Immediate(last.clone());
                    }
                }
                if let Some(rest) = self.take_next_chain_block() {
                    return FeedResult::Immediate(rest);
                }
                if let Some(last) = &self.last_sent {
                    return FeedResult::Immediate(last.clone());
                }
                self.bad_block()
            }
            Some(Pcb::I { ns, more, nr: _ }) => {
                let _ = nad;
                if ns != self.host_ns {
                    // Sequence violation → request retransmission (§11.5.3).
                    return FeedResult::Immediate(block(r_pcb(self.host_ns, 0), &[]));
                }
                if inf.len() > self.host_ifsd {
                    return self.bad_block();
                }
                if self.recv_chain.len() + inf.len() > MAX_RELAY {
                    return self.bad_block();
                }
                let _ = self.recv_chain.extend_from_slice(inf);
                if more {
                    // §11.6.3: ACK chain pieces with R(N(R)) = same ns.
                    return FeedResult::Immediate(block(r_pcb(ns, 0), &[]));
                }
                self.host_ns ^= 1;
                let apdu = core::mem::replace(&mut self.recv_chain, Vec::new());
                FeedResult::Apdu(apdu)
            }
            None => self.bad_block(),
        }
    }

    fn on_s_request(&mut self, kind: SKind, inf: &[u8]) -> FeedResult {
        match kind {
            SKind::Resync => {
                // §11.5.3.2: reset and reply S(RESYNC response).
                self.ns = 0;
                self.host_ns = 0;
                self.recv_chain.clear();
                self.send_chain.clear();
                self.wtx_pending = false;
                let rsp = block(s_pcb(SKind::Resync, true), &[]);
                self.last_sent = Some(rsp.clone());
                FeedResult::Immediate(rsp)
            }
            SKind::Ifs => {
                let new_ifs = *inf.first().unwrap_or(&32) as usize;
                // Host announces its receive size: bound OUR sends by it.
                self.send_ifsc = new_ifs.min(MAX_IFSC);
                let rsp = block(s_pcb(SKind::Ifs, true), inf);
                self.last_sent = Some(rsp.clone());
                FeedResult::Immediate(rsp)
            }
            SKind::Abort => {
                self.recv_chain.clear();
                let rsp = block(s_pcb(SKind::Abort, true), inf);
                self.last_sent = Some(rsp.clone());
                FeedResult::Immediate(rsp)
            }
            SKind::Wtx => {
                // Host-requested WTX is unusual (initiator-side); echo the
                // response to keep the exchange legal.
                let rsp = block(s_pcb(SKind::Wtx, true), inf);
                self.last_sent = Some(rsp.clone());
                FeedResult::Immediate(rsp)
            }
        }
    }

    fn on_s_response(&mut self, kind: SKind, inf: &[u8]) -> FeedResult {
        match kind {
            SKind::Wtx if self.wtx_pending => {
                // Host granted the WTX: send the deferred first block.
                self.wtx_pending = false;
                if let Some(b) = self.take_next_chain_block() {
                    return FeedResult::Immediate(b);
                }
                self.bad_block()
            }
            _ => {
                let _ = inf;
                // Unsolicited response: retransmit last (interop-safe).
                if let Some(last) = &self.last_sent {
                    FeedResult::Immediate(last.clone())
                } else {
                    self.bad_block()
                }
            }
        }
    }

    /// Frame a card response. `took_ms` drives the WTX dance: if the
    /// card exceeded the host's BWT budget, return S(WTX request m)
    /// first; the actual I-block goes out when the host's WTX response
    /// (or any next XfrBlock) arrives.
    pub fn card_response(&mut self, rsp: &[u8], took_ms: u32) -> Vec<u8, 520> {
        // Stage the full response as the outgoing chain.
        self.send_chain.clear();
        let cap = rsp.len().min(MAX_RELAY);
        let _ = self.send_chain.extend_from_slice(&rsp[..cap]);

        if took_ms > 150 {
            // §11.6.2 WTX: buy m × BWT before our I-block appears.
            self.wtx_pending = true;
            let wtx = block(s_pcb(SKind::Wtx, false), &[2]);
            self.last_sent = Some(wtx.clone());
            return wtx;
        }
        self.take_next_chain_block()
            .unwrap_or_else(|| self.final_empty_block())
    }

    /// Empty-payload final I-block: still a real final block — toggles
    /// N(S) (§11.6.3) and latches last_sent for §11.5.3.3 retransmit, or
    /// every later block runs one ns behind the host engine.
    fn final_empty_block(&mut self) -> Vec<u8, 520> {
        let b = block(i_pcb(self.ns, false, self.host_ns), &[]);
        self.ns ^= 1;
        self.last_sent = Some(b.clone());
        b
    }

    /// Next outgoing chain block (first call after staging, then one
    /// per host R-ACK).
    fn take_next_chain_block(&mut self) -> Option<Vec<u8, 520>> {
        if self.send_chain.is_empty() {
            return None;
        }
        let chunk = self.send_ifsc.min(self.send_chain.len());
        let more = self.send_chain.len() > chunk;
        let mut inf: heapless::Vec<u8, MAX_RELAY> = Vec::new();
        let _ = inf.extend_from_slice(&self.send_chain[..chunk]);
        for _ in 0..chunk {
            self.send_chain.remove(0);
        }
        let b = block(i_pcb(self.ns, more, self.host_ns), &inf);
        if !more {
            // §11.6.3: N(S) toggles after the final block of the chain.
            self.ns ^= 1;
        }
        self.last_sent = Some(b.clone());
        Some(b)
    }

    fn bad_block(&mut self) -> FeedResult {
        // §11.5.3: answer with R(N(R), parity-error) demanding retransmit.
        FeedResult::Immediate(block(r_pcb(self.host_ns, 1), &[]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_i(ns: u8, more: bool, nr: u8, inf: &[u8]) -> Vec<u8, 520> {
        block(i_pcb(ns, more, nr), inf)
    }

    #[test]
    fn pcb_bit_layout_locked_to_iso_7816_3_table_45() {
        // GREAT EXPECTATION: byte literals, not builder calls — the
        // pre-7f136a0 bug (R(1) emitted 0x88, reserved bit set) was
        // invisible to tests that asserted against r_pcb() itself.
        assert_eq!(r_pcb(0, 0), 0x80);
        assert_eq!(r_pcb(0, 1), 0x81);
        assert_eq!(r_pcb(1, 0), 0x90);
        assert_eq!(r_pcb(1, 1), 0x91);
        assert_eq!(i_pcb(0, false, 0), 0x00);
        assert_eq!(i_pcb(0, false, 1), 0x10);
        assert_eq!(i_pcb(0, true, 0), 0x20);
        assert_eq!(i_pcb(1, false, 0), 0x40);
        match parse_pcb(0x90) {
            Some(Pcb::R { nr, err }) => assert_eq!((nr, err), (1, 0)),
            other => panic!("0x90 must decode as R(1, 0), got {other:?}"),
        }
        match parse_pcb(0x10) {
            Some(Pcb::I { ns, more, nr }) => assert_eq!((ns, more, nr), (0, false, 1)),
            other => panic!("0x10 must decode as I(0, M=0, nr=1), got {other:?}"),
        }
    }

    #[test]
    fn chain_overflow_answers_r_other_error() {
        // G-03 guard: the piece that would cross MAX_RELAY is rejected
        // with R(N(R), other-error=1) — never silently truncated.
        let mut ep = T1Endpoint::new();
        let piece = [0xAA; 32];
        for _ in 0..MAX_RELAY / 32 {
            match ep.feed(&host_i(0, true, 0, &piece)) {
                FeedResult::Immediate(r) => assert_eq!(r[1], r_pcb(0, 0)),
                other => panic!("expected chain ACK, got {other:?}"),
            }
        }
        match ep.feed(&host_i(0, true, 0, &[0xBB])) {
            FeedResult::Immediate(r) => assert_eq!(r[1], r_pcb(0, 1)),
            other => panic!("expected R(other-error) on overflow, got {other:?}"),
        }
    }

    #[test]
    fn ifs_negotiation_is_answered_locally() {
        // libccid always opens with S(IFS request FE) (bench strace,
        // 2026-10-09): `00 C1 01 FE` + LRC.
        let mut ep = T1Endpoint::new();
        let fe = FeedResult::Immediate(block(s_pcb(SKind::Ifs, true), &[0xFE]));
        assert_eq!(ep.feed(&[0x00, 0xC1, 0x01, 0xFE, 0x3E]), fe);
        assert_eq!(ep.send_ifsc, 254);
    }

    #[test]
    fn single_apdu_roundtrip_and_sequence_toggle() {
        let mut ep = T1Endpoint::new();
        // host I(0, M=0) carrying a SELECT
        let sel = [0x00, 0xA4, 0x04, 0x00, 0x02, 0x3F, 0x00];
        match ep.feed(&host_i(0, false, 0, &sel)) {
            FeedResult::Apdu(a) => assert_eq!(a.as_slice(), &sel),
            other => panic!("expected Apdu, got {other:?}"),
        }
        let rsp = ep.card_response(&[0x90, 0x00], 5);
        // our I(0, nr=1) reply: N(R) = next expected host ns = 1
        assert_eq!(rsp[1], i_pcb(0, false, 1));
        assert_eq!(&rsp[3..5], &[0x90, 0x00]);
        assert_eq!(lrc(&rsp[..rsp.len() - 1]), rsp[rsp.len() - 1]);

        // second exchange: host ns toggled to 1, our ns to 1
        let get = [0x00, 0xCA, 0x00, 0x6E];
        match ep.feed(&host_i(1, false, 0, &get)) {
            FeedResult::Apdu(a) => assert_eq!(a.as_slice(), &get),
            other => panic!("expected Apdu, got {other:?}"),
        }
        let rsp2 = ep.card_response(&[0x02, 0x01, 0x02, 0x90, 0x00], 5);
        assert_eq!(rsp2[1], i_pcb(1, false, 0));
    }

    #[test]
    fn host_chain_is_acked_then_assembled() {
        let mut ep = T1Endpoint::new();
        // §11.6.3: chain pieces share N(S); M=1 until the last.
        let p1 = vec![0xAA; 32];
        let p2 = vec![0xBB; 8];
        match ep.feed(&host_i(0, true, 0, &p1)) {
            FeedResult::Immediate(r) => assert_eq!(r[1], r_pcb(0, 0)),
            other => panic!("expected R-ACK, got {other:?}"),
        }
        match ep.feed(&host_i(0, false, 0, &p2)) {
            FeedResult::Apdu(mut a) => {
                a.truncate(40);
                assert_eq!(a.as_slice(), &[p1.as_slice(), p2.as_slice()].concat());
            }
            other => panic!("expected Apdu, got {other:?}"),
        }
        // next host block must carry the toggled ns
        match ep.feed(&host_i(1, false, 0, &[0x00])) {
            FeedResult::Apdu(_) => {}
            other => panic!("expected Apdu after toggle, got {other:?}"),
        }
    }

    #[test]
    fn sequence_violation_requests_retransmission() {
        let mut ep = T1Endpoint::new();
        // host sends ns=1 while we expect 0
        match ep.feed(&host_i(1, false, 0, &[0x00])) {
            FeedResult::Immediate(r) => assert_eq!(r[1], r_pcb(0, 0)),
            other => panic!("expected R, got {other:?}"),
        }
    }

    #[test]
    fn oversized_card_response_is_sent_as_chain() {
        let mut ep = T1Endpoint::new();
        ep.feed(&[0x00, 0xC1, 0x01, 0xFE, 0x3E]); // IFS → 254
        ep.feed(&host_i(0, false, 0, &[0x00])); // trigger
        let rsp = [0xAB; 300];
        let first = ep.card_response(&rsp, 5);
        assert_eq!(first[1], i_pcb(0, true, 1), "first block: M=1, 254B INF");
        assert_eq!(first[2], 254);

        // host R-ACKs → second block finishes the chain
        let ack = block(r_pcb(0, 0), &[]);
        match ep.feed(&ack) {
            FeedResult::Immediate(second) => {
                assert_eq!(second[1], i_pcb(0, false, 1));
                assert_eq!(second[2], 46);
            }
            other => panic!("expected chain tail, got {other:?}"),
        }
        // our ns toggled for the next exchange
        ep.feed(&host_i(1, false, 0, &[0x00]));
        let tail = ep.card_response(&[0x90, 0x00], 5);
        assert_eq!(tail[1], i_pcb(1, false, 0));
    }

    #[test]
    fn r_block_retransmits_last() {
        let mut ep = T1Endpoint::new();
        ep.feed(&host_i(0, false, 0, &[0x00]));
        let first = ep.card_response(&[0x90, 0x00], 5);
        // host asks for retransmission (§11.5.3.3)
        match ep.feed(&block(r_pcb(0, 0), &[])) {
            FeedResult::Immediate(again) => assert_eq!(again, first),
            other => panic!("expected retransmit, got {other:?}"),
        }
    }

    /// pcscd gp-install death sequence (bench 2026-10-11, issue #59):
    /// a >150ms exchange does the S(WTX) dance, then the host sends a
    /// CHAINED I-block pair (33-byte INSTALL[FOR LOAD] > ifsc=32). The
    /// whole dance must keep ns/nr bookkeeping per proto-t1.c rules.
    #[test]
    fn wtx_dance_then_chained_apdu_keeps_sequence() {
        let mut ep = T1Endpoint::new();
        // slow exchange: host I(0) → card answer staged, WTX requested
        match ep.feed(&host_i(0, false, 0, &[0x80, 0x50, 0x00, 0x00, 0x08])) {
            FeedResult::Apdu(_) => {}
            other => panic!("expected Apdu, got {other:?}"),
        }
        let wtx = ep.card_response(&[0x00, 0x00, 0x90, 0x00], 200);
        assert_eq!(wtx[1], s_pcb(SKind::Wtx, false));

        // host grants S(WTX response 02) → deferred I-block(ns=0, nr=1)
        let grant = block(s_pcb(SKind::Wtx, true), &[0x02]);
        match ep.feed(&grant) {
            FeedResult::Immediate(r) => assert_eq!(r[1], i_pcb(0, false, 1)),
            other => panic!("expected deferred I-block, got {other:?}"),
        }

        // chained 33-byte APDU: I#1(ns=1, M=1, 32B) → R-ACK
        let p1 = vec![0x84, 0xE6, 0x02, 0x00, 0x1B];
        let p1b = [p1.as_slice(), &[0xAA; 27][..]].concat();
        match ep.feed(&host_i(1, true, 0, &p1b[..32])) {
            FeedResult::Immediate(r) => assert_eq!(r[1], r_pcb(1, 0)),
            other => panic!("expected chain R-ACK, got {other:?}"),
        }
        // I#2(ns=1, M=0, 1B) → assembled 33-byte APDU
        match ep.feed(&host_i(1, false, 0, &[0xBB])) {
            FeedResult::Apdu(a) => assert_eq!(a.len(), 33),
            other => panic!("expected assembled Apdu, got {other:?}"),
        }
        // fast answer: I(ns=1, nr=0) — ns must be 1 (toggled once, above)
        let rsp = ep.card_response(&[0x90, 0x00], 30);
        assert_eq!(rsp[1], i_pcb(1, false, 0));

        // and the NEXT exchange still alternates cleanly
        match ep.feed(&host_i(0, false, 1, &[0x00, 0xC0, 0x00, 0x00, 0x00])) {
            FeedResult::Apdu(_) => {}
            other => panic!("expected Apdu after dance, got {other:?}"),
        }
        let tail = ep.card_response(&[0x90, 0x00], 5);
        assert_eq!(tail[1], i_pcb(0, false, 1));
    }

    /// An empty card payload (failed exchange answered by an error
    /// DataBlock) must still consume the ns slot and set last_sent, or
    /// every later block is one ns behind the host engine.
    #[test]
    fn empty_card_response_toggles_ns_and_sets_last_sent() {
        let mut ep = T1Endpoint::new();
        match ep.feed(&host_i(0, false, 0, &[0x00])) {
            FeedResult::Apdu(_) => {}
            other => panic!("expected Apdu, got {other:?}"),
        }
        let empty = ep.card_response(&[], 30);
        assert_eq!(empty[1], i_pcb(0, false, 1));

        // retransmit request must repeat that very block (§11.5.3.3)
        match ep.feed(&block(r_pcb(0, 0), &[])) {
            FeedResult::Immediate(again) => assert_eq!(again, empty),
            other => panic!("expected retransmit of empty I-block, got {other:?}"),
        }

        // next exchange: host ns=1; our reply must carry ns=1
        match ep.feed(&host_i(1, false, 0, &[0x00])) {
            FeedResult::Apdu(_) => {}
            other => panic!("expected Apdu, got {other:?}"),
        }
        let rsp = ep.card_response(&[0x90, 0x00], 5);
        assert_eq!(rsp[1], i_pcb(1, false, 0));
    }

    #[test]
    fn resync_resets_state() {
        let mut ep = T1Endpoint::new();
        ep.feed(&host_i(0, false, 0, &[0x00]));
        ep.card_response(&[0x90, 0x00], 5);
        match ep.feed(&[0x00, 0xC0, 0x00, 0xC0]) {
            FeedResult::Immediate(r) => assert_eq!(r[1], s_pcb(SKind::Resync, true)),
            other => panic!("expected D0, got {other:?}"),
        }
        // after resync both sides restart at ns=0
        match ep.feed(&host_i(0, false, 0, &[0x00])) {
            FeedResult::Apdu(_) => {}
            other => panic!("expected Apdu post-resync, got {other:?}"),
        }
        let r = ep.card_response(&[0x90, 0x00], 5);
        assert_eq!(r[1], i_pcb(0, false, 1));
    }

    #[test]
    fn bad_lrc_demands_retransmission() {
        let mut ep = T1Endpoint::new();
        let mut b = host_i(0, false, 0, &[0x00]);
        let n = b.len();
        b[n - 1] ^= 0xFF; // corrupt LRC
        match ep.feed(&b) {
            FeedResult::Immediate(r) => assert_eq!(r[1] & 0x80, 0x80, "R-block"),
            other => panic!("expected R, got {other:?}"),
        }
    }

    #[test]
    fn slow_card_runs_wtx_dance() {
        let mut ep = T1Endpoint::new();
        ep.feed(&host_i(0, false, 0, &[0x00]));
        // §11.6.2: >BWT → S(WTX request m) first, I-block after the grant
        let wtx = ep.card_response(&[0x90, 0x00], 400);
        assert_eq!(wtx[1], s_pcb(SKind::Wtx, false));
        assert_eq!(wtx[3], 2);
        match ep.feed(&[0x00, 0xE3, 0x01, 0x02, 0xE0]) {
            FeedResult::Immediate(b) => {
                assert_eq!(b[1], i_pcb(0, false, 1));
                assert_eq!(&b[3..5], &[0x90, 0x00]);
            }
            other => panic!("expected deferred I-block, got {other:?}"),
        }
    }
}

/// Monotonic microseconds for the WTX decision (§11.6.2): the card path
/// can run seconds (on-card RSA) — beyond any host BWT.
#[cfg(any(target_arch = "xtensa", target_arch = "riscv32"))]
pub fn now_us() -> i64 {
    unsafe { esp_idf_sys::esp_timer_get_time() }
}

#[cfg(any(target_arch = "xtensa", target_arch = "riscv32"))]
pub fn elapsed_ms_since(started_us: i64) -> u32 {
    ((unsafe { esp_idf_sys::esp_timer_get_time() } - started_us) / 1000).max(0) as u32
}

#[cfg(not(any(target_arch = "xtensa", target_arch = "riscv32")))]
pub fn now_us() -> i64 {
    0
}

#[cfg(not(any(target_arch = "xtensa", target_arch = "riscv32")))]
pub fn elapsed_ms_since(_started_us: i64) -> u32 {
    0
}

/// True for an S-block REQUEST (RESYNC/IFS/ABORT/WTX) — the reader's own
/// link-layer duty per ISO 7816-3 §11.5.2: negotiation is answered by the
/// reader itself and does not require an active card session. I-blocks and
/// R-blocks DO need the card (APDU payload / retransmission of a card-bound
/// response) and stay gated on PresentActive.
pub fn is_s_block_request(bytes: &[u8]) -> bool {
    // Block layout §11.3: NAD(1) | PCB(1) | LEN(1) | INF | LRC.
    if bytes.len() < 4 {
        return false;
    }
    let pcb = bytes[1];
    // S-block: 11 b5 0 kind — request has bit 4 clear (response sets it).
    (pcb & 0xC0) == 0xC0 && (pcb & 0x10) == 0
}
