//! USB-CDC log shim (issue #91): restores log output after the
//! USB-Serial/JTAG driver is claimed by the CCID serving loop.
//!
//! On the nucula (ESP32-C3) the console shares the USB-CDC port with CCID.
//! From the moment `UsbSerialDriver::new` claims the peripheral, direct
//! console writes are silently dropped — every diagnostic logged by the
//! serving path (`power_on failed: …` in `ccid_handler`, the PN7160 init
//! ladder, the mains) vanished, which is exactly what issue #91 reports.
//!
//! Two output paths must be captured:
//!
//! - **C-level `ESP_LOGx`** — routed through ESP-IDF's swappable vprintf
//!   sink. [`install`] replaces it via `esp_log_set_vprintf`; the sink
//!   formats each chunk with `vsnprintf` and appends it to the ring.
//!   IDF 5.5's text formatter calls the sink up to three times per line
//!   (header chunk, message chunk, `"\n"`), so the ring assembles lines
//!   from arbitrary byte chunks and only exposes newline-terminated
//!   lines.
//! - **Rust `log` facade** — esp-idf-svc 0.52's `EspLogger` writes via
//!   `fwrite` to newlib stdout, NOT through the vprintf sink, so the
//!   shim also installs its own `log::Log` sink ([`init`], replacing
//!   `EspLogger::initialize_default` — the log crate accepts exactly one
//!   global sink, first installer wins). Pre-claim lines still go to
//!   stdout (the FWID boot marker keeps landing on the console);
//!   post-claim they route into the ring.
//!
//! The main loop drains the ring onto the claimed driver at safe points
//! (read-idle, post-response). Host-side GemPC parsers scan for
//! SYNC-anchored LRC-validated frames and tolerate the interleaved log
//! text — proven by the conformance battery and the m5stick, which has
//! always logged mid-stream.
//!
//! Queue semantics (drop-oldest, line boundaries, bounded lines) live in
//! the host-tested [`LogRing`] below, mirroring the `ble_log_queue`
//! extraction pattern; the esp-idf shell is target-gated like the other
//! target modules.

/// Total ring capacity: 4 KB of retained log lines.
pub const RING_CAPACITY: usize = 4096;

/// Hard per-line cap, newline included (matches the BLE debug console's
/// line budget). Longer input lines are head-truncated. The cap also
/// guarantees a growing partial line always fits once all complete lines
/// have been evicted (`MAX_LINE_LEN < RING_CAPACITY`).
pub const MAX_LINE_LEN: usize = 200;

/// Byte ring of complete log lines.
///
/// Live data is kept contiguous at `buf[head..head + used]`: zero or more
/// complete (newline-terminated) lines followed by at most one partial
/// line. A line becomes drainable only once its `\n` has been pushed.
/// Overflow drops the OLDEST complete lines — the newest diagnostics are
/// the ones that explain a stall — and counts the evictions.
pub struct LogRing {
    buf: [u8; RING_CAPACITY],
    head: usize,
    /// Live bytes: complete lines plus the trailing partial line.
    used: usize,
    /// Length of the trailing incomplete line (0 right after a newline).
    partial: usize,
    /// Complete lines evicted by overflow since the last `take_dropped`.
    dropped: u32,
}

impl Default for LogRing {
    fn default() -> Self {
        Self::new()
    }
}

impl LogRing {
    pub const fn new() -> Self {
        Self {
            buf: [0; RING_CAPACITY],
            head: 0,
            used: 0,
            partial: 0,
            dropped: 0,
        }
    }

    /// Append raw log bytes. The bytes need not be line-aligned: chunks
    /// accumulate onto the partial tail and complete a line when their
    /// `\n` arrives. Never blocks, never panics, never fails.
    pub fn push_bytes(&mut self, chunk: &[u8]) {
        for &b in chunk {
            self.push_byte(b);
        }
    }

    /// Queue one already-complete line (convenience for callers that
    /// format whole lines, like the Rust `log` sink).
    pub fn push_line(&mut self, line: &[u8]) {
        self.push_bytes(line);
        if self.partial != 0 {
            // Caller omitted the terminator; complete the line ourselves
            // so it cannot glue onto the next one.
            self.push_byte(b'\n');
        }
    }

    fn push_byte(&mut self, b: u8) {
        // Mid-line truncation: keep the head of over-long lines and
        // swallow bytes until the newline completes the line. The payload
        // cap keeps every COMPLETE line at `MAX_LINE_LEN` bytes or fewer,
        // newline included.
        if self.partial + 1 >= MAX_LINE_LEN && b != b'\n' {
            return;
        }
        if self.used >= RING_CAPACITY {
            self.evict_oldest();
        }
        if self.used >= RING_CAPACITY {
            // Unreachable while MAX_LINE_LEN < RING_CAPACITY (a partial
            // line alone cannot fill the ring); drop rather than panic.
            return;
        }
        if self.head + self.used == RING_CAPACITY {
            self.compact();
        }
        self.buf[self.head + self.used] = b;
        self.used += 1;
        if b == b'\n' {
            self.partial = 0;
        } else {
            self.partial += 1;
        }
    }

    /// Pop the oldest complete line (newline included) into `out`.
    /// Returns the number of bytes copied — never 0, a complete line has
    /// at least its newline — or `None` when no complete line is pending.
    /// The line is consumed even if `out` is too short to hold it whole.
    pub fn pop_line_into(&mut self, out: &mut [u8]) -> Option<usize> {
        let len = self.oldest_complete_len()?;
        let n = len.min(out.len());
        out[..n].copy_from_slice(&self.buf[self.head..self.head + n]);
        self.head += len;
        self.used -= len;
        if self.used == 0 {
            self.head = 0;
        }
        Some(n)
    }

    /// Whether a complete line is ready to pop.
    pub fn has_line(&self) -> bool {
        self.used > self.partial
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0
    }

    /// Complete + partial bytes currently retained.
    pub fn used(&self) -> usize {
        self.used
    }

    /// Complete lines evicted by overflow since the last take.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Take (and reset) the overflow-eviction counter, for the
    /// "N lines dropped" marker the drain emits.
    pub fn take_dropped(&mut self) -> u32 {
        core::mem::replace(&mut self.dropped, 0)
    }

    /// Length (newline included) of the oldest complete line.
    fn oldest_complete_len(&self) -> Option<usize> {
        let complete = self.used - self.partial;
        if complete == 0 {
            return None;
        }
        let nl = self.buf[self.head..self.head + complete]
            .iter()
            .position(|&b| b == b'\n')?;
        Some(nl + 1)
    }

    /// Drop the oldest complete line, counting the eviction.
    fn evict_oldest(&mut self) {
        if let Some(len) = self.oldest_complete_len() {
            self.head += len;
            self.used -= len;
            self.dropped += 1;
            if self.used == 0 {
                self.head = 0;
            }
        }
    }

    /// Slide live data back to offset 0 so appending never wraps.
    fn compact(&mut self) {
        if self.head == 0 {
            return;
        }
        self.buf.copy_within(self.head..self.head + self.used, 0);
        self.head = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drain until empty, returning the lines (newline included).
    fn drain_all(ring: &mut LogRing) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = [0u8; MAX_LINE_LEN];
        while let Some(n) = ring.pop_line_into(&mut buf) {
            out.push(buf[..n].to_vec());
        }
        out
    }

    fn line(payload: &str) -> Vec<u8> {
        format!("{payload}\n").into_bytes()
    }

    #[test]
    fn partial_line_is_not_poppable_until_newline() {
        let mut ring = LogRing::new();
        ring.push_bytes(b"power_on failed: Communication");
        assert!(!ring.is_empty());
        assert!(!ring.has_line(), "no newline yet — no complete line");
        // Not newline-terminated: nothing to pop yet.
        let mut out = [0u8; MAX_LINE_LEN];
        assert_eq!(ring.pop_line_into(&mut out), None);
        // Split across chunks like the IDF sink chunks (header/message/\n).
        ring.push_bytes(b" timeout");
        assert_eq!(ring.pop_line_into(&mut out), None);
        ring.push_bytes(b"\n");
        let n = ring.pop_line_into(&mut out).expect("line complete");
        assert_eq!(&out[..n], b"power_on failed: Communication timeout\n");
        assert!(ring.is_empty());
        assert!(!ring.has_line());
    }

    #[test]
    fn multiple_newlines_in_one_chunk_complete_multiple_lines() {
        let mut ring = LogRing::new();
        ring.push_bytes(b"one\ntwo\nthree");
        let lines = drain_all(&mut ring);
        assert_eq!(lines, vec![line("one"), line("two")]);
        // "three" still partial.
        assert_eq!(ring.used(), 5);
        ring.push_bytes(b"\n");
        assert_eq!(drain_all(&mut ring), vec![line("three")]);
    }

    #[test]
    fn push_line_completes_terminatorless_lines() {
        let mut ring = LogRing::new();
        ring.push_line(b"no newline");
        ring.push_line(b"with newline\n");
        assert_eq!(
            drain_all(&mut ring),
            vec![line("no newline"), line("with newline")]
        );
    }

    #[test]
    fn overflow_drops_oldest_keeps_newest() {
        let mut ring = LogRing::new();
        let line_len = 100; // 99 payload bytes + '\n'
        let payload = |i: usize| format!("line-{i:04}{}", "x".repeat(line_len - 1 - 9));
        let total_lines = RING_CAPACITY / line_len + 10; // force evictions
        for i in 0..total_lines {
            ring.push_line(payload(i).as_bytes());
        }
        let kept = drain_all(&mut ring);
        assert_eq!(
            kept.len() + ring.dropped() as usize,
            total_lines,
            "every line either survived or was counted"
        );
        assert!(kept.len() <= RING_CAPACITY / line_len);
        // Survivors are the NEWEST lines, in order, byte-intact.
        let first_kept = total_lines - kept.len();
        for (idx, l) in kept.iter().enumerate() {
            assert_eq!(l.len(), line_len);
            assert!(l.starts_with(format!("line-{:04}", first_kept + idx).as_bytes()));
        }
    }

    #[test]
    fn dropped_counter_resets_on_take() {
        let mut ring = LogRing::new();
        assert_eq!(ring.take_dropped(), 0);
        // Fill with one big partial (no newline) then a flood of lines to
        // force evictions.
        ring.push_bytes(&[b'a'; MAX_LINE_LEN]);
        ring.push_bytes(b"partial-no-newline");
        for i in 0..(RING_CAPACITY / 10) {
            ring.push_line(format!("flush-{i:03}").as_bytes());
        }
        assert!(ring.dropped() > 0, "evictions happened");
        let before = ring.dropped();
        assert_eq!(ring.take_dropped(), before, "take returns the count");
        assert_eq!(ring.dropped(), 0, "take resets the counter");
    }

    #[test]
    fn over_long_line_is_truncated_but_terminated() {
        let mut ring = LogRing::new();
        ring.push_bytes(&[b'x'; MAX_LINE_LEN + 50]);
        ring.push_bytes(b"tail-that-must-be-swallowed");
        ring.push_bytes(b"\n");
        let lines = drain_all(&mut ring);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].len(),
            MAX_LINE_LEN,
            "capped at MAX_LINE_LEN, newline included"
        );
        assert_eq!(lines[0].last(), Some(&b'\n'));
        assert!(ring.is_empty());
    }

    #[test]
    fn wraparound_keeps_bytes_intact() {
        // 100-byte lines totalling well past the capacity force head drift
        // and compaction; every surviving line must come back byte-identical.
        let payload = |i: usize| format!("wrap-{i:03}-{}", "y".repeat(90));
        let total = 60;
        let mut ring = LogRing::new();
        for i in 0..total {
            ring.push_line(payload(i).as_bytes());
        }
        let kept = drain_all(&mut ring);
        assert!(kept.len() < total, "evictions happened");
        assert_eq!(kept.len() + ring.dropped() as usize, total);
        // Survivors are the consecutive NEWEST lines, byte-intact, in order.
        let first_kept = total - kept.len();
        for (idx, l) in kept.iter().enumerate() {
            assert_eq!(l, format!("{}\n", payload(first_kept + idx)).as_bytes());
        }
    }

    #[test]
    fn short_pop_buffer_consumes_line() {
        let mut ring = LogRing::new();
        ring.push_line(b"0123456789");
        let mut tiny = [0u8; 4];
        assert_eq!(ring.pop_line_into(&mut tiny), Some(4));
        assert_eq!(&tiny, b"0123");
        // Line fully consumed; ring empty despite the truncated copy.
        assert!(ring.is_empty());
        assert_eq!(ring.pop_line_into(&mut tiny), None);
    }

    #[test]
    fn empty_ring_pops_none() {
        let mut ring = LogRing::new();
        let mut out = [0u8; 8];
        assert_eq!(ring.pop_line_into(&mut out), None);
        assert!(!ring.has_line());
        ring.push_bytes(b"\n"); // bare newline is a complete (empty) line
        assert_eq!(ring.pop_line_into(&mut out), Some(1));
        assert_eq!(out[0], b'\n');
    }
}

#[cfg(all(
    feature = "pn7160-ccid",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
mod imp {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    use esp_idf_hal::delay::TickType;
    use esp_idf_hal::usb_serial::UsbSerialDriver;

    use super::{LogRing, MAX_LINE_LEN};
    use crate::ble_log_queue::format_record;

    /// Stack buffer for one sink call's formatted output. Log lines longer
    /// than this (header + message) arrive as a truncated chunk; the ring's
    /// per-line cap bounds the stored line anyway.
    const FMT_BUF_LEN: usize = 256;

    /// Per-write timeout while draining onto the claimed CDC driver.
    const LOG_WRITE_TIMEOUT_MS: u64 = 50;

    /// Lines per drain call, bounding worst-case idle-path time when no
    /// host is consuming (leftover lines drain on the next pass).
    const DRAIN_BURST: usize = 8;

    static RING: Mutex<LogRing> = Mutex::new(LogRing::new());

    /// Flipped by [`install`]: routes the Rust `log` facade into the ring
    /// instead of newlib stdout (which is dead once the driver is claimed).
    static ROUTED: AtomicBool = AtomicBool::new(false);

    // Not exposed by esp-idf-sys; declared locally with the bindgen
    // `va_list` alias so the sink's va_list round-trips ABI-identically
    // (bench-verified: xtensa passes the 12-byte va_list by value in
    // a3..a5, riscv32 passes a plain pointer).
    extern "C" {
        fn vsnprintf(
            s: *mut core::ffi::c_char,
            n: usize,
            format: *const core::ffi::c_char,
            args: esp_idf_sys::va_list,
        ) -> core::ffi::c_int;
    }

    fn with_ring<R>(f: impl FnOnce(&mut LogRing) -> R) -> R {
        // Poison-tolerant: a panic in a previous critical section must not
        // kill logging forever.
        let mut guard = RING.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    /// The vprintf sink (C-level `ESP_LOGx`): format one chunk into a
    /// stack buffer and append it to the ring. Called in task context and
    /// required to be re-entrant — the ring lock is the only shared state
    /// and it is held only for the push, never across console/console-adjacent
    /// calls, so the sink never blocks meaningfully and never panics.
    unsafe extern "C" fn shim_vprintf(
        format: *const core::ffi::c_char,
        args: esp_idf_sys::va_list,
    ) -> core::ffi::c_int {
        let mut fmt_buf = [0u8; FMT_BUF_LEN];
        let n = unsafe { vsnprintf(fmt_buf.as_mut_ptr().cast(), FMT_BUF_LEN, format, args) };
        if n > 0 {
            let len = (n as usize).min(FMT_BUF_LEN - 1);
            with_ring(|ring| ring.push_bytes(&fmt_buf[..len]));
        }
        n
    }

    /// The `log` facade sink (Rust `log::warn!` etc.). Replaces
    /// `EspLogger` — which writes to newlib stdout and therefore dies at
    /// the driver claim — while preserving its pre-claim behaviour so the
    /// FWID boot marker still reaches the console.
    struct ShimLogger;

    static SHIM_LOGGER: ShimLogger = ShimLogger;

    impl log::Log for ShimLogger {
        fn enabled(&self, _metadata: &log::Metadata) -> bool {
            true
        }

        fn log(&self, record: &log::Record) {
            let line = format_record(
                &record.level().to_string(),
                record.target(),
                &record.args().to_string(),
            );
            if ROUTED.load(Ordering::Relaxed) {
                with_ring(|ring| ring.push_line(&line));
            } else {
                // Pre-claim: newlib stdout still reaches the console.
                let mut out = std::io::stdout();
                let _ = out.write_all(&line);
                let _ = out.flush();
            }
        }

        fn flush(&self) {}
    }

    /// Install the `log` facade sink. Call at main start INSTEAD of
    /// `EspLogger::initialize_default()` (the log crate accepts exactly
    /// one global sink, first installer wins).
    pub fn init(level: log::LevelFilter) {
        log::set_logger(&SHIM_LOGGER).expect("log shim: global logger already installed");
        log::set_max_level(level);
    }

    /// Route all further log output into the ring: swap IDF's vprintf
    /// sink (catches C-level `ESP_LOGx`) and flip the Rust facade
    /// routing flag. Call AFTER `UsbSerialDriver::new` — from that moment
    /// direct console writes are dropped and the drained ring is the only
    /// way log output leaves the chip.
    pub fn install() {
        // SAFETY: installs a process-global C callback that only ever
        // locks the ring mutex and copies bytes — it never blocks on the
        // console it replaces, so it cannot deadlock against IDF's
        // stdout-locked log formatter.
        unsafe { esp_idf_sys::esp_log_set_vprintf(Some(shim_vprintf)) };
        ROUTED.store(true, Ordering::Release);
    }

    /// Write pending complete lines to the claimed USB-CDC driver.
    /// Called from the serving loop (read-idle, post-response). A line
    /// whose write fails (no host consuming) is dropped — diagnostics,
    /// not protocol state. Intermixing with CCID frames is safe: the
    /// host-side parsers scan for SYNC-anchored LRC-validated frames.
    pub fn drain_into(usb: &mut UsbSerialDriver) {
        let timeout = TickType::new_millis(LOG_WRITE_TIMEOUT_MS).ticks();
        let dropped = with_ring(LogRing::take_dropped);
        if dropped > 0 {
            let marker = format!("=== log shim: {dropped} lines dropped ===\n");
            let _ = usb.write(marker.as_bytes(), timeout);
        }
        let mut line = [0u8; MAX_LINE_LEN];
        for _ in 0..DRAIN_BURST {
            let Some(n) = with_ring(|ring| ring.pop_line_into(&mut line)) else {
                break;
            };
            match usb.write(&line[..n], timeout) {
                Ok(w) if w == n => {}
                _ => break, // host not consuming — stop this pass
            }
        }
    }
}

#[cfg(not(all(
    feature = "pn7160-ccid",
    any(target_arch = "xtensa", target_arch = "riscv32")
)))]
mod imp {
    // Host / non-pn7160-ccid stub: the esp-idf shell only exists on
    // esp-idf targets. Nothing references it here; it keeps the module
    // self-consistent (ble_logger pattern).
    pub fn init(level: log::LevelFilter) {
        let _ = level;
    }

    pub fn install() {}

    pub fn drain_into<T>(sink: &mut T) {
        let _ = sink;
    }
}

pub use imp::*;
