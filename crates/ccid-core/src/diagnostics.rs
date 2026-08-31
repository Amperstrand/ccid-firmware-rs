//! Runtime diagnostics counters for the CCID firmware.
//!
//! The [`Diagnostics`] struct is serializable to a fixed-size little-endian
//! byte layout so it can be returned to the host over the CCID Escape vendor
//! command (see issue #29). It is intentionally dependency-free and `no_std`.
//!
//! Serialized layout (28 bytes, all little-endian):
//!
//! | Offset | Field | Type |
//! |--------|-------|------|
//! | 0-3 | `apdu_tx_count` | `u32` LE |
//! | 4-7 | `apdu_rx_count` | `u32` LE |
//! | 8-11 | `nak_count` | `u32` LE |
//! | 12-15 | `error_count` | `u32` LE |
//! | 16-19 | `reinit_count` | `u32` LE |
//! | 20-23 | `card_present` | `u32` LE (0 or 1) |
//! | 24-27 | `uptime_ticks` | `u32` LE |

/// Runtime diagnostics counters for the CCID reader firmware.
///
/// Each counter tracks a runtime event that is useful for field diagnostics
/// and host-side health monitoring. The struct is serialized to a fixed 28-byte
/// little-endian representation via [`Diagnostics::to_bytes`] and deserialized
/// via [`Diagnostics::from_bytes`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// Number of APDUs transmitted to the card.
    pub apdu_tx_count: u32,
    /// Number of APDUs received from the card.
    pub apdu_rx_count: u32,
    /// Number of NAKs received from the card (T=1 protocol).
    pub nak_count: u32,
    /// Number of protocol/transport errors encountered.
    pub error_count: u32,
    /// Number of times the card session was re-initialised.
    pub reinit_count: u32,
    /// Whether a card is currently present in the slot.
    pub card_present: bool,
    /// Firmware uptime in ticks (resolution is board-specific).
    pub uptime_ticks: u32,
}

impl Diagnostics {
    /// Serialized size in bytes: 7 fields × 4 bytes = 28.
    pub const SERIALIZED_SIZE: usize = 28;

    /// Create a zero-initialised diagnostics struct.
    pub const fn new() -> Self {
        Self {
            apdu_tx_count: 0,
            apdu_rx_count: 0,
            nak_count: 0,
            error_count: 0,
            reinit_count: 0,
            card_present: false,
            uptime_ticks: 0,
        }
    }

    /// Serialize to little-endian bytes.
    ///
    /// Writes exactly [`SERIALIZED_SIZE`] (28) bytes into `buf`. The
    /// `card_present` flag is encoded as a `u32` little-endian slot at offset
    /// 20 (value 0 or 1) so the entire struct is a uniform array of `u32` LE
    /// words and trivially parseable on the host.
    ///
    /// # Returns
    /// Number of bytes written (always 28), or 0 if `buf` is shorter than
    /// [`SERIALIZED_SIZE`].
    pub fn to_bytes(&self, buf: &mut [u8]) -> usize {
        if buf.len() < Self::SERIALIZED_SIZE {
            return 0;
        }

        let card_present_u32: u32 = self.card_present as u32;

        buf[0..4].copy_from_slice(&self.apdu_tx_count.to_le_bytes());
        buf[4..8].copy_from_slice(&self.apdu_rx_count.to_le_bytes());
        buf[8..12].copy_from_slice(&self.nak_count.to_le_bytes());
        buf[12..16].copy_from_slice(&self.error_count.to_le_bytes());
        buf[16..20].copy_from_slice(&self.reinit_count.to_le_bytes());
        buf[20..24].copy_from_slice(&card_present_u32.to_le_bytes());
        buf[24..28].copy_from_slice(&self.uptime_ticks.to_le_bytes());

        Self::SERIALIZED_SIZE
    }

    /// Deserialize from little-endian bytes.
    ///
    /// Returns `None` if `buf` is shorter than [`SERIALIZED_SIZE`] (28 bytes).
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SERIALIZED_SIZE {
            return None;
        }

        let read_u32 = |offset: usize| -> u32 {
            let arr: [u8; 4] = buf[offset..offset + 4].try_into().unwrap_or([0u8; 4]);
            u32::from_le_bytes(arr)
        };

        Some(Self {
            apdu_tx_count: read_u32(0),
            apdu_rx_count: read_u32(4),
            nak_count: read_u32(8),
            error_count: read_u32(12),
            reinit_count: read_u32(16),
            card_present: read_u32(20) != 0,
            uptime_ticks: read_u32(24),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_zero() {
        let d = Diagnostics::new();
        assert_eq!(
            d,
            Diagnostics {
                apdu_tx_count: 0,
                apdu_rx_count: 0,
                nak_count: 0,
                error_count: 0,
                reinit_count: 0,
                card_present: false,
                uptime_ticks: 0,
            }
        );
        assert_eq!(d, Diagnostics::default());
    }

    #[test]
    fn serialized_size_constant_is_28() {
        assert_eq!(Diagnostics::SERIALIZED_SIZE, 28);
    }

    #[test]
    fn round_trip_default() {
        let original = Diagnostics::new();
        let mut buf = [0u8; Diagnostics::SERIALIZED_SIZE];
        let written = original.to_bytes(&mut buf);
        assert_eq!(written, Diagnostics::SERIALIZED_SIZE);
        let restored = Diagnostics::from_bytes(&buf).expect("round-trip default");
        assert_eq!(restored, original);
    }

    #[test]
    fn round_trip_with_values() {
        let original = Diagnostics {
            apdu_tx_count: 0x0000_0001,
            apdu_rx_count: 0x0000_0002,
            nak_count: 0x0000_0003,
            error_count: 0x0000_0004,
            reinit_count: 0x0000_0005,
            card_present: true,
            uptime_ticks: 0x1234_5678,
        };
        let mut buf = [0u8; Diagnostics::SERIALIZED_SIZE];
        let written = original.to_bytes(&mut buf);
        assert_eq!(written, Diagnostics::SERIALIZED_SIZE);
        let restored = Diagnostics::from_bytes(&buf).expect("round-trip with values");
        assert_eq!(restored, original);
    }

    #[test]
    fn round_trip_all_max_values() {
        let original = Diagnostics {
            apdu_tx_count: u32::MAX,
            apdu_rx_count: u32::MAX,
            nak_count: u32::MAX,
            error_count: u32::MAX,
            reinit_count: u32::MAX,
            card_present: true,
            uptime_ticks: u32::MAX,
        };
        let mut buf = [0u8; Diagnostics::SERIALIZED_SIZE];
        let written = original.to_bytes(&mut buf);
        assert_eq!(written, Diagnostics::SERIALIZED_SIZE);
        let restored = Diagnostics::from_bytes(&buf).expect("round-trip max");
        assert_eq!(restored, original);
    }

    #[test]
    fn from_bytes_rejects_short_buffer() {
        let short = [0u8; 27];
        assert!(Diagnostics::from_bytes(&short).is_none());
        let exact = [0u8; 28];
        assert!(Diagnostics::from_bytes(&exact).is_some());
    }

    #[test]
    fn from_bytes_accepts_longer_buffer() {
        let mut buf = [0u8; 64];
        buf[20] = 0x01;
        let d = Diagnostics::from_bytes(&buf).expect("longer buffer parses");
        assert!(d.card_present);
    }

    #[test]
    fn card_present_serializes_as_u32_le() {
        let d = Diagnostics {
            card_present: true,
            ..Diagnostics::new()
        };
        let mut buf = [0u8; Diagnostics::SERIALIZED_SIZE];
        d.to_bytes(&mut buf);
        assert_eq!(&buf[20..24], &[0x01, 0x00, 0x00, 0x00]);

        let d_false = Diagnostics {
            card_present: false,
            ..Diagnostics::new()
        };
        let mut buf2 = [0u8; Diagnostics::SERIALIZED_SIZE];
        buf2[20..24].copy_from_slice(&[0xFF; 4]);
        d_false.to_bytes(&mut buf2);
        assert_eq!(&buf2[20..24], &[0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn field_offsets_are_exact() {
        let d = Diagnostics {
            apdu_tx_count: 0x0A,
            apdu_rx_count: 0x0B,
            nak_count: 0x0C,
            error_count: 0x0D,
            reinit_count: 0x0E,
            card_present: true,
            uptime_ticks: 0x0F,
        };
        let mut buf = [0u8; Diagnostics::SERIALIZED_SIZE];
        d.to_bytes(&mut buf);

        assert_eq!(&buf[0..4], 0x0Au32.to_le_bytes());
        assert_eq!(&buf[4..8], 0x0Bu32.to_le_bytes());
        assert_eq!(&buf[8..12], 0x0Cu32.to_le_bytes());
        assert_eq!(&buf[12..16], 0x0Du32.to_le_bytes());
        assert_eq!(&buf[16..20], 0x0Eu32.to_le_bytes());
        assert_eq!(&buf[20..24], 1u32.to_le_bytes());
        assert_eq!(&buf[24..28], 0x0Fu32.to_le_bytes());
    }

    #[test]
    fn to_bytes_rejects_short_buffer() {
        let d = Diagnostics::new();
        let mut short = [0u8; 27];
        assert_eq!(d.to_bytes(&mut short), 0);
    }

    #[test]
    fn to_bytes_writes_exactly_28() {
        let d = Diagnostics {
            apdu_tx_count: 42,
            uptime_ticks: 999,
            ..Diagnostics::new()
        };
        let mut buf = [0xAA; 40];
        let written = d.to_bytes(&mut buf);
        assert_eq!(written, 28);
        assert_eq!(&buf[28..40], &[0xAA; 12]);
    }

    #[test]
    fn golden_byte_exact_serialization() {
        // This test pins the frozen 28-byte wire format for CCID Escape 0xD0.
        // Any change to the layout would break host compatibility.
        // (Ported from amp-diagnostics — the only test written there that
        // never lived in this repo — before amp-embedded-common was archived.)
        let d = Diagnostics {
            apdu_tx_count: 0x0000_0123,
            apdu_rx_count: 0x0000_4567,
            nak_count: 0x0000_89AB,
            error_count: 0x0000_CDEF,
            reinit_count: 0x0000_FEDC,
            card_present: true,
            uptime_ticks: 0x1234_5678,
        };

        let mut buf = [0u8; Diagnostics::SERIALIZED_SIZE];
        let written = d.to_bytes(&mut buf);
        assert_eq!(written, 28);

        // Byte-exact golden output (little-endian, each field at fixed offset):
        // 0x00 0x01 0x23 0x00  (apdu_tx_count: 0x00000123)
        // 0x67 0x45 0x00 0x00  (apdu_rx_count: 0x00004567)
        // 0xAB 0x89 0x00 0x00  (nak_count: 0x000089AB)
        // 0xEF 0xCD 0x00 0x00  (error_count: 0x0000CDEF)
        // 0xDC 0xFE 0x00 0x00  (reinit_count: 0x0000FEDC)
        // 0x01 0x00 0x00 0x00  (card_present: true = 1)
        // 0x78 0x56 0x34 0x12  (uptime_ticks: 0x12345678)
        assert_eq!(
            buf,
            [
                0x23, 0x01, 0x00, 0x00, // apdu_tx_count
                0x67, 0x45, 0x00, 0x00, // apdu_rx_count
                0xAB, 0x89, 0x00, 0x00, // nak_count
                0xEF, 0xCD, 0x00, 0x00, // error_count
                0xDC, 0xFE, 0x00, 0x00, // reinit_count
                0x01, 0x00, 0x00, 0x00, // card_present (true)
                0x78, 0x56, 0x34, 0x12, // uptime_ticks
            ]
        );

        // Round-trip to verify deserialization matches the frozen format
        let restored = Diagnostics::from_bytes(&buf).expect("golden round-trip");
        assert_eq!(restored, d);
    }
}
