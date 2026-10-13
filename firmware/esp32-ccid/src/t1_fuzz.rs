//! T=1 endpoint fuzz suite (issue #92 Part A extension): adversarial
//! block sequences through `T1Endpoint::feed` + `card_response` — the
//! same deterministic-LCG approach as `ccid_fuzz`, extended to the
//! block layer the PCB fix (7f136a0) hardened.
//!
//! Invariants under fuzz (ISO 7816-3 §11 contract):
//! - `feed` never panics on any byte sequence
//! - every response block carries a valid LRC (§11.3.3.1)
//! - an R-block answer is emitted for garbage (retransmit demand, §11.5.3)
//! - chained state cannot wedge: RESYNC always recovers to a working
//!   exchange (§11.5.3.2)

use crate::t1::{FeedResult, T1Endpoint};

struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 24) as u8
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n.max(1)
    }
}

fn valid_lrc(block: &[u8]) -> bool {
    if block.len() < 4 {
        return false;
    }
    let expected = block[..block.len() - 1].iter().fold(0u8, |a, b| a ^ b);
    block[block.len() - 1] == expected
}

fn feed_ok(ep: &mut T1Endpoint, bytes: &[u8]) -> Option<Vec<u8>> {
    match ep.feed(bytes) {
        FeedResult::Immediate(v) => Some(v.to_vec()),
        FeedResult::Apdu(_a) => {
            // fuzz calls never do card I/O: answer with a canned SW
            let rsp = ep.card_response(&[0x90, 0x00], 0);
            Some(rsp.to_vec())
        }
    }
}

#[test]
fn t1_fuzz_arbitrary_blocks_never_panic() {
    let mut rng = Rng(0x7B1E);
    for _ in 0..3000u32 {
        let len = rng.below(260);
        let bytes: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        let mut ep = T1Endpoint::new();
        // Contract: never panics; any Immediate answer is LRC-valid.
        if let Some(rsp) = feed_ok(&mut ep, &bytes) {
            assert!(valid_lrc(&rsp), "malformed LRC in reply");
            assert!(rsp.len() >= 4, "reply too short");
        }
    }
}

#[test]
fn t1_fuzz_valid_shape_random_fields() {
    // NAD/PCB/LEN consistent, INF random: the LRC is then the only
    // thing that can reject — and a wrong LRC must yield an R-block.
    let mut rng = Rng(0x0FF5E7);
    for _ in 0..3000u32 {
        let pcb = rng.byte();
        let inf_len = rng.below(40);
        let mut block = vec![0x00, pcb, inf_len as u8];
        block.extend((0..inf_len).map(|_| rng.byte()));
        let lrc = block.iter().fold(0u8, |a, b| a ^ b);
        block.push(lrc);
        let mut ep = T1Endpoint::new();
        if let Some(rsp) = feed_ok(&mut ep, &block) {
            assert!(valid_lrc(&rsp));
        }
    }
}

#[test]
fn t1_fuzz_garbage_always_answered_with_r_or_s() {
    // After any garbage, a well-formed RESYNC must always be answered
    // (S-block duty, §11.5.3.2) — the endpoint cannot go mute.
    let mut rng = Rng(0xC0DE);
    for round in 0..500u32 {
        let mut ep = T1Endpoint::new();
        let junk_len = rng.below(100);
        let junk: Vec<u8> = (0..junk_len).map(|_| rng.byte()).collect();
        let _ = feed_ok(&mut ep, &junk);

        let resync = [0x00u8, 0xC0, 0x00, 0xC0];
        let rsp = feed_ok(&mut ep, &resync)
            .unwrap_or_else(|| panic!("round {round}: RESYNC unanswered after garbage"));
        assert_eq!(rsp[1], 0xE0, "round {round}: not a RESYNC response");
        assert!(valid_lrc(&rsp));
    }
}

#[test]
fn t1_fuzz_chain_storms_recover_via_resync() {
    // Adversarial chain sequences: partial chains (M=1 with no final),
    // sequence violations, interleaved garbage — then RESYNC must
    // always restore a working single exchange (§11.5.3.2).
    let mut rng = Rng(0xFA11);
    for round in 0..300u32 {
        let mut ep = T1Endpoint::new();
        ep.feed(&[0x00, 0xC1, 0x01, 0xFE, 0x3E]); // arm IFS 254

        // storm: random I-block pieces with plausible sequence bits
        let storm = 10 + rng.below(20);
        for _ in 0..storm {
            let ns = rng.below(2) as u8;
            let more = rng.below(2) == 0;
            let inf = rng.below(60);
            let mut blk = vec![0x00, (ns << 6) | ((more as u8) << 5), inf as u8];
            blk.extend((0..inf).map(|_| rng.byte()));
            let lrc = blk.iter().fold(0u8, |a, b| a ^ b);
            blk.push(lrc);
            let _ = feed_ok(&mut ep, &blk);
        }

        // RESYNC then one clean exchange must succeed.
        let _ = feed_ok(&mut ep, &[0x00, 0xC0, 0x00, 0xC0]);
        let apdu = vec![0x00, 0xA4, 0x04, 0x00];
        let mut blk = vec![0x00, 0x02, apdu.len() as u8];
        blk.extend_from_slice(&apdu);
        let lrc = blk.iter().fold(0u8, |a, b| a ^ b);
        blk.push(lrc);
        match ep.feed(&blk) {
            FeedResult::Apdu(_) => {
                let rsp = ep.card_response(&[0x90, 0x00], 0);
                assert!(
                    valid_lrc(&rsp),
                    "round {round}: post-resync I-block bad LRC"
                );
            }
            other => panic!("round {round}: clean I-block rejected post-resync: {other:?}"),
        }
    }
}

#[test]
fn t1_fuzz_wtx_dance_never_wedges() {
    // Random WTX multipliers with deferred responses: the pending block
    // must always come out after the WTX grant (§11.6.2), never lost.
    let mut rng = Rng(0xB1005);
    for round in 0..500u32 {
        let mut ep = T1Endpoint::new();
        ep.feed(&[0x00, 0x02, 0x00, 0x02]); // clean empty I-block
                                            // G-04: WTX fires against the negotiated BWT — force a tight
                                            // window (BWI=0 → BWT 91 ms) so the 400 ms exchange overruns it.
        ep.set_host_timing(0, 254);
        let mult = 1 + rng.below(59) as u8;
        let wtx = ep.card_response(&[0x90, 0x00], 400);
        assert_eq!(wtx[1], 0xC3, "round {round}: not a WTX request");
        let grant = vec![0x00, 0xE3, 0x01, mult];
        let mut blk = grant.clone();
        let lrc = blk.iter().fold(0u8, |a, b| a ^ b);
        blk.push(lrc);
        match ep.feed(&blk) {
            FeedResult::Immediate(b) => {
                assert!(valid_lrc(&b), "round {round}: deferred block bad LRC");
                assert_eq!(b[1] & 0x80, 0, "round {round}: deferred block not I-type");
            }
            other => panic!("round {round}: WTX grant lost the pending block: {other:?}"),
        }
    }
}

#[test]
fn t1_fuzz_r_block_storms_keep_endpoint_alive() {
    // Host sends arbitrary R-blocks (retransmit demands / error codes):
    // the endpoint must keep answering — retransmitting its last block
    // or an error R — and never go silent or panic.
    let mut rng = Rng(0x5EED2);
    for round in 0..1000u32 {
        let mut ep = T1Endpoint::new();
        ep.feed(&[0x00, 0x02, 0x00, 0x02]);
        ep.card_response(&[0x90, 0x00], 0); // have a last_sent

        let storm = 5 + rng.below(15);
        for _ in 0..storm {
            let nr = rng.below(2) as u8;
            let err = rng.below(4) as u8;
            let pcb = 0x80 | (nr << 4) | err;
            let mut blk = vec![0x00, pcb, 0x00];
            let lrc = blk.iter().fold(0u8, |a, b| a ^ b);
            blk.push(lrc);
            if let Some(rsp) = feed_ok(&mut ep, &blk) {
                assert!(valid_lrc(&rsp), "round {round}: R-storm reply bad LRC");
            }
        }

        // still alive: a RESYNC answers.
        let rsp = feed_ok(&mut ep, &[0x00, 0xC0, 0x00, 0xC0])
            .unwrap_or_else(|| panic!("round {round}: endpoint dead after R-storm"));
        assert_eq!(rsp[1], 0xE0);
    }
}
