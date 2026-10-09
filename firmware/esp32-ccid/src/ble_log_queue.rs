//! Pure log-line ring buffer shared by the BLE debug logger (issue #66).
//!
//! Target-independent by design so the queue semantics — drop-oldest under
//! overflow, bounded line length, the "attached" banner with its dropped
//! count — stay host-testable in CI, mirroring the `ccid_serial_server`
//! extraction pattern. The ESP-only `ble_logger` shell wraps this; the
//! flow-control model follows the NimBLEStream reference (ring buffer,
//! never block the logger, DROP_OLDER_DATA on overflow plus a dropped
//! counter) adapted to this firmware's pull-based drain loop.

use std::collections::VecDeque;

/// Maximum encoded length of one queued line (matches the GATT
/// characteristic `max_len`; `ble_debug::MAX_LOG_CHUNK`).
pub const MAX_LOG_LINE_LEN: usize = 200;

/// Queued lines retained while no central is subscribed. Drop-oldest under
/// overflow keeps the NEWEST diagnostics — the lines that explain a stall
/// matter more than the boot banner that preceded it.
const QUEUE_CAPACITY: usize = 32;

pub struct LogQueue {
    lines: VecDeque<Vec<u8>>,
    dropped: u32,
}

impl Default for LogQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl LogQueue {
    pub fn new() -> Self {
        Self {
            lines: VecDeque::with_capacity(QUEUE_CAPACITY),
            dropped: 0,
        }
    }

    /// Queue one already-encoded line, dropping the OLDEST line (and
    /// counting it) when the ring is full. Never blocks, never fails.
    pub fn push_line(&mut self, line: Vec<u8>) {
        if self.lines.len() >= QUEUE_CAPACITY {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(line);
    }

    pub fn pop_front(&mut self) -> Option<Vec<u8>> {
        self.lines.pop_front()
    }

    pub fn front(&self) -> Option<&[u8]> {
        self.lines.front().map(|line| line.as_slice())
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Lines dropped by overflow since the last banner was taken.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Take the "central attached" banner: reports how many oldest lines
    /// were lost while nobody was listening, then resets the counter so a
    /// later re-attach reports only the gap since.
    pub fn take_banner(&mut self) -> Vec<u8> {
        let dropped = self.dropped;
        self.dropped = 0;
        if dropped == 0 {
            b"=== BLE log attached ===\n".to_vec()
        } else {
            format!("=== BLE log attached; {dropped} older lines dropped ===\n").into_bytes()
        }
    }
}

/// Encode a log record as `[LEVEL] module: message\n`, truncated to
/// `MAX_LOG_LINE_LEN` while always keeping the trailing newline so a
/// truncated line never glues onto the next one on the central's terminal.
pub fn format_record(level: &str, module: &str, message: &str) -> Vec<u8> {
    let mut rendered = format!("[{level}] {module}: {message}\n").into_bytes();

    if rendered.len() > MAX_LOG_LINE_LEN {
        rendered.truncate(MAX_LOG_LINE_LEN.saturating_sub(1));
        if rendered.last().copied() != Some(b'\n') {
            rendered.push(b'\n');
        }
    }

    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: usize) -> Vec<u8> {
        format!("line-{n}\n").into_bytes()
    }

    #[test]
    fn push_then_pop_is_fifo() {
        let mut q = LogQueue::new();
        q.push_line(line(1));
        q.push_line(line(2));
        assert_eq!(q.pop_front(), Some(line(1)));
        assert_eq!(q.pop_front(), Some(line(2)));
        assert!(q.is_empty());
    }

    #[test]
    fn overflow_drops_oldest_and_counts() {
        let mut q = LogQueue::new();
        for n in 0..QUEUE_CAPACITY + 7 {
            q.push_line(line(n));
        }
        assert_eq!(q.len(), QUEUE_CAPACITY);
        assert_eq!(q.dropped(), 7);
        // The survivors are the NEWEST lines.
        assert_eq!(q.pop_front(), Some(line(7)));
        assert_eq!(q.front(), Some(line(8).as_slice()));
    }

    #[test]
    fn banner_reports_and_resets_dropped_counter() {
        let mut q = LogQueue::new();
        let banner = q.take_banner();
        assert_eq!(banner, b"=== BLE log attached ===\n");

        for n in 0..QUEUE_CAPACITY + 3 {
            q.push_line(line(n));
        }
        let banner = q.take_banner();
        assert_eq!(banner, b"=== BLE log attached; 3 older lines dropped ===\n");
        // Counter reset: an immediate re-attach reports a clean ring.
        assert_eq!(q.take_banner(), b"=== BLE log attached ===\n");
    }

    #[test]
    fn format_matches_legacy_encoding() {
        let encoded = format_record("WARN", "esp32_ccid::wifi", "connect failed");
        assert_eq!(encoded, b"[WARN] esp32_ccid::wifi: connect failed\n");
    }

    #[test]
    fn format_truncates_but_keeps_newline() {
        let long = "x".repeat(500);
        let encoded = format_record("INFO", "m", &long);
        assert_eq!(encoded.len(), MAX_LOG_LINE_LEN);
        assert_eq!(encoded.last(), Some(&b'\n'));
        // No newline mid-message means no mid-character concerns: bytes only.
        assert!(encoded.starts_with(b"[INFO] m: xxxx"));
    }

    #[test]
    fn format_short_line_untouched() {
        let encoded = format_record("ERROR", "m", "e");
        assert_eq!(encoded, b"[ERROR] m: e\n");
    }

    #[test]
    fn format_boundary_length_is_exact() {
        // A message that lands exactly on the cap must not lose its newline.
        let pad = "y".repeat(MAX_LOG_LINE_LEN - b"[INFO] m: \n".len());
        let encoded = format_record("INFO", "m", &pad);
        assert_eq!(encoded.len(), MAX_LOG_LINE_LEN);
        assert_eq!(encoded.last(), Some(&b'\n'));
    }
}
