//! Turnaround timing counters surfaced via CCID Escape 0xD2 (issue #120).
//!
//! Splits the per-command latency budget into the two halves the perf
//! lane needs: serve-path turnaround (last command byte received →
//! response write complete, stamped by the UART serve loop in `main.rs`)
//! and the card exchange (time inside `transmit_apdu`, stamped by
//! `CardDriverAdapter`). Escape 0xD0's 28-byte counter format is
//! untouched — this is a separate 16-byte payload.
//!
//! Serialized layout (16 bytes, all `u32` little-endian):
//!
//! | Offset | Field | Type |
//! |--------|-------|------|
//! | 0-3 | `last_turnaround_us` | `u32` LE |
//! | 4-7 | `max_turnaround_us` | `u32` LE |
//! | 8-11 | `last_card_exchange_us` | `u32` LE |
//! | 12-15 | `max_card_exchange_us` | `u32` LE |
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
}

impl TurnaroundStats {
    pub const SERIALIZED_SIZE: usize = 16;

    pub const fn new() -> Self {
        Self {
            last_turnaround_us: 0,
            max_turnaround_us: 0,
            last_card_exchange_us: 0,
            max_card_exchange_us: 0,
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
        let mut buf = [0u8; TurnaroundStats::SERIALIZED_SIZE];
        assert_eq!(s.to_bytes(&mut buf), 16);
        let back = TurnaroundStats::from_bytes(&buf).unwrap();
        assert_eq!(back, s);
        // LE word order: first word is last_turnaround_us.
        assert_eq!(u32::from_le_bytes(buf[0..4].try_into().unwrap()), 4_250_000);
    }

    #[test]
    fn serialization_rejects_short_buffer_both_ways() {
        let s = TurnaroundStats::new();
        let mut buf = [0u8; 15];
        assert_eq!(s.to_bytes(&mut buf), 0);
        assert!(TurnaroundStats::from_bytes(&buf).is_none());
    }

    #[test]
    fn fresh_stats_serialize_to_zeros() {
        let s = TurnaroundStats::new();
        let mut buf = [0u8; 16];
        s.to_bytes(&mut buf);
        assert!(buf.iter().all(|&b| b == 0));
    }
}
