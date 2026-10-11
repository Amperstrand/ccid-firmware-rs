//! Turnaround timing counters surfaced via CCID Escape 0xD2 (issue #120).
//!
//! Splits the per-command latency budget into the two halves the perf
//! lane needs: serve-path turnaround (last command byte received →
//! response write complete, stamped by the UART serve loop in `main.rs`)
//! and the card exchange (time inside `transmit_apdu`, stamped by
//! `CardDriverAdapter`). Escape 0xD0's 28-byte counter format is
//! untouched — this is a separate payload.
//!
//! Serialized layout (32 bytes, all `u32` little-endian):
//!
//! | Offset | Field | Type |
//! |--------|-------|------|
//! | 0-3 | `last_turnaround_us` | `u32` LE |
//! | 4-7 | `max_turnaround_us` | `u32` LE |
//! | 8-11 | `last_card_exchange_us` | `u32` LE |
//! | 12-15 | `max_card_exchange_us` | `u32` LE |
//! | 16-19 | `last_exchange_blocks` | `u32` LE |
//! | 20-23 | `last_exchange_wtx_replies` | `u32` LE |
//! | 24-27 | `last_exchange_recoveries` | `u32` LE |
//! | 28-31 | `last_exchange_max_transceive_us` | `u32` LE |
//!
//! Words 4-8 decompose the card exchange (bench 2026-10-11: minimal
//! exchange 18.2 ms, +0.77 ms per response byte — the totals alone
//! cannot separate card wait from WTX negotiation from link recovery):
//! blocks = every transceive of the last exchange, wtx_replies =
//! outgoing S-blocks (ISO 14443-4 §7.5.2 S(WTX) answers the card sent
//! first), recoveries = outgoing R-blocks (R(ACK)/R(NAK), §7.5.5),
//! max_transceive_us = the longest single block round trip.
//!
//! Host probe: `tests/hardware/nfc/turnaround_probe.py`.

/// Last/max turnaround + card-exchange durations, µs resolution
/// (`esp_timer` on target; zeroed on host builds where no clock is
/// injected — host tests exercise the recording methods directly).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnaroundStats {
    /// Last complete command: last command byte received → response
    /// write complete (echo + notify + response flushed to the wire).
    last_turnaround_us: u32,
    /// Maximum observed turnaround since boot.
    max_turnaround_us: u32,
    /// Last `transmit_apdu` card exchange duration.
    last_card_exchange_us: u32,
    /// Maximum card exchange since boot.
    max_card_exchange_us: u32,
    /// Decomposition of the last card exchange (words 4-8, see module
    /// docs). Zeroed until an instrumented backend records one.
    last_exchange_blocks: u32,
    last_exchange_wtx_replies: u32,
    last_exchange_recoveries: u32,
    last_exchange_max_transceive_us: u32,
}

/// Outgoing block classification by PCB (ISO 14443-4 §7.4: bits b8 b7 —
/// 00 I-block, 10 R-block, 11 S-block; bench citation: the reader sends
/// S-blocks during an exchange only as S(WTX) replies, R-blocks only as
/// §7.5.5 recovery, everything else is I-block data).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutgoingBlockKind {
    IBlock,
    RBlock,
    SBlock,
}

impl OutgoingBlockKind {
    pub fn from_pcb(pcb: u8) -> Option<Self> {
        match pcb >> 6 {
            0b00 => Some(Self::IBlock),
            0b10 => Some(Self::RBlock),
            0b11 => Some(Self::SBlock),
            _ => None, // b8b7=01 is reserved (ISO 14443-4 Table 3)
        }
    }
}

impl TurnaroundStats {
    pub const SERIALIZED_SIZE: usize = 32;

    pub const fn new() -> Self {
        Self {
            last_turnaround_us: 0,
            max_turnaround_us: 0,
            last_card_exchange_us: 0,
            max_card_exchange_us: 0,
            last_exchange_blocks: 0,
            last_exchange_wtx_replies: 0,
            last_exchange_recoveries: 0,
            last_exchange_max_transceive_us: 0,
        }
    }

    /// Serve-loop stamp: one per served command (Respond path).
    pub fn record_turnaround_us(&mut self, us: u32) {
        self.last_turnaround_us = us;
        self.max_turnaround_us = self.max_turnaround_us.max(us);
    }

    /// Card-exchange stamp: one per `transmit_apdu` call.
    pub fn record_card_exchange_us(&mut self, us: u32) {
        self.last_card_exchange_us = us;
        self.max_card_exchange_us = self.max_card_exchange_us.max(us);
    }

    /// Decomposition stamp for the last `transmit_apdu` (module docs
    /// words 4-8). Call once per exchange, after the counting wrapper
    /// finished.
    pub fn record_exchange_decomposition(
        &mut self,
        blocks: u32,
        wtx_replies: u32,
        recoveries: u32,
        max_transceive_us: u32,
    ) {
        self.last_exchange_blocks = blocks;
        self.last_exchange_wtx_replies = wtx_replies;
        self.last_exchange_recoveries = recoveries;
        self.last_exchange_max_transceive_us = max_transceive_us;
    }

    pub fn last_turnaround_us(&self) -> u32 {
        self.last_turnaround_us
    }

    pub fn max_turnaround_us(&self) -> u32 {
        self.max_turnaround_us
    }

    pub fn last_card_exchange_us(&self) -> u32 {
        self.last_card_exchange_us
    }

    pub fn max_card_exchange_us(&self) -> u32 {
        self.max_card_exchange_us
    }

    pub fn last_exchange_decomposition(&self) -> (u32, u32, u32, u32) {
        (
            self.last_exchange_blocks,
            self.last_exchange_wtx_replies,
            self.last_exchange_recoveries,
            self.last_exchange_max_transceive_us,
        )
    }

    /// Serialize to little-endian bytes (exactly
    /// [`SERIALIZED_SIZE`] bytes, or 0 if `buf` is too short).
    pub fn to_bytes(&self, buf: &mut [u8]) -> usize {
        if buf.len() < Self::SERIALIZED_SIZE {
            return 0;
        }
        buf[0..4].copy_from_slice(&self.last_turnaround_us.to_le_bytes());
        buf[4..8].copy_from_slice(&self.max_turnaround_us.to_le_bytes());
        buf[8..12].copy_from_slice(&self.last_card_exchange_us.to_le_bytes());
        buf[12..16].copy_from_slice(&self.max_card_exchange_us.to_le_bytes());
        buf[16..20].copy_from_slice(&self.last_exchange_blocks.to_le_bytes());
        buf[20..24].copy_from_slice(&self.last_exchange_wtx_replies.to_le_bytes());
        buf[24..28].copy_from_slice(&self.last_exchange_recoveries.to_le_bytes());
        buf[28..32].copy_from_slice(&self.last_exchange_max_transceive_us.to_le_bytes());
        Self::SERIALIZED_SIZE
    }

    /// Deserialize from little-endian bytes (`None` if too short).
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SERIALIZED_SIZE {
            return None;
        }
        let word = |off: usize| -> u32 {
            u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0u8; 4]))
        };
        Some(Self {
            last_turnaround_us: word(0),
            max_turnaround_us: word(4),
            last_card_exchange_us: word(8),
            max_card_exchange_us: word(12),
            last_exchange_blocks: word(16),
            last_exchange_wtx_replies: word(20),
            last_exchange_recoveries: word(24),
            last_exchange_max_transceive_us: word(28),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_tracks_last_and_max_turnaround() {
        let mut s = TurnaroundStats::new();
        s.record_turnaround_us(1_500);
        s.record_turnaround_us(2_000);
        s.record_turnaround_us(900);
        assert_eq!(s.last_turnaround_us(), 900);
        assert_eq!(s.max_turnaround_us(), 2_000);
    }

    #[test]
    fn record_tracks_last_and_max_card_exchange() {
        let mut s = TurnaroundStats::new();
        s.record_card_exchange_us(35_000);
        s.record_card_exchange_us(1_200);
        assert_eq!(s.last_card_exchange_us(), 1_200);
        assert_eq!(s.max_card_exchange_us(), 35_000);
    }

    #[test]
    fn serialization_round_trip() {
        let mut s = TurnaroundStats::new();
        s.record_turnaround_us(4_250_000);
        s.record_card_exchange_us(38_100);
        s.record_exchange_decomposition(3, 2, 1, 12_345);
        let mut buf = [0u8; TurnaroundStats::SERIALIZED_SIZE];
        assert_eq!(s.to_bytes(&mut buf), 32);
        let back = TurnaroundStats::from_bytes(&buf).unwrap();
        assert_eq!(back, s);
        // LE word order: first word is last_turnaround_us.
        assert_eq!(u32::from_le_bytes(buf[0..4].try_into().unwrap()), 4_250_000);
        // Words 4-8: blocks, wtx, recoveries, max single transceive.
        assert_eq!(u32::from_le_bytes(buf[16..20].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(buf[20..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(buf[24..28].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(buf[28..32].try_into().unwrap()),
            12_345
        );
    }

    #[test]
    fn serialization_rejects_short_buffer_both_ways() {
        let s = TurnaroundStats::new();
        let mut buf = [0u8; 31];
        assert_eq!(s.to_bytes(&mut buf), 0);
        assert!(TurnaroundStats::from_bytes(&buf).is_none());
    }

    #[test]
    fn fresh_stats_serialize_to_zeros() {
        let s = TurnaroundStats::new();
        let mut buf = [0u8; 32];
        s.to_bytes(&mut buf);
        assert!(buf.iter().all(|&b| b == 0));
    }

    /// PCB classification vectors: I-block 0x02 (this firmware's no-CID
    /// I-block), R(ACK) 0xA2 / R(NAK) 0xB2, S(WTX) 0xF2 / S(DESELECT)
    /// 0xC2 — encodings per iso14443::type_a::pcb tests (ISO 14443-4
    /// §7.4 Table 3).
    #[test]
    fn outgoing_block_kind_from_pcb() {
        assert_eq!(OutgoingBlockKind::from_pcb(0x02), Some(OutgoingBlockKind::IBlock));
        assert_eq!(OutgoingBlockKind::from_pcb(0x03), Some(OutgoingBlockKind::IBlock));
        assert_eq!(OutgoingBlockKind::from_pcb(0xA2), Some(OutgoingBlockKind::RBlock));
        assert_eq!(OutgoingBlockKind::from_pcb(0xB2), Some(OutgoingBlockKind::RBlock));
        assert_eq!(OutgoingBlockKind::from_pcb(0xF2), Some(OutgoingBlockKind::SBlock));
        assert_eq!(OutgoingBlockKind::from_pcb(0xC2), Some(OutgoingBlockKind::SBlock));
        assert_eq!(OutgoingBlockKind::from_pcb(0x40), None);
        assert_eq!(OutgoingBlockKind::from_pcb(0x00), Some(OutgoingBlockKind::IBlock));
    }
}
