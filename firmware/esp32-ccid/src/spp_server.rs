//! pcscd-over-Bluetooth-SPP: BT Classic Serial Port Profile transport for
//! the GemPC serial CCID server.
//!
//! SPP is a raw byte pipe to the host — `rfcomm watch` on the BlueZ side
//! exposes `/dev/rfcomm0`, libccidtwin opens it like any serial port, and
//! the same strict GemPC framing rides RFCOMM that rode UART0. The serving
//! core is therefore reused VERBATIM ([`crate::ccid_serial_server::
//! CcidSerialServer`] + [`crate::ccid_handler::CcidHandler`], issue #90
//! DRY); this module only adds:
//!
//! - a pure, host-tested serving pump ([`SppPump`]) that preserves the
//!   `serve_uart` wire cadence exactly (echo → notify → response, NAK on
//!   malformed frames, interval-gated polls, read-idle parser reset);
//! - the callback→loop handoff ([`SppRxRing`], [`SppLink`]): SPP data
//!   arrives on the Bluedroid callback task, card I/O must NOT run there
//!   (#89: MFRC522 polls block for hundreds of ms), so bytes cross a
//!   lock-guarded ring and everything slow stays on the main task;
//! - the target shell (`mod shell`, `spp` feature) that brings up
//!   `BtDriver<BtClassic>` + `EspGap` + `EspSpp` per the reference below.
//!
//! # REFERENCE (Reference-First doctrine)
//!
//! esp-rs/esp-idf-svc `examples/bt_spp_acceptor.rs` (crate version 0.52.1
//! as consumed here; byte-identical SPP/GAP API at master `c8bcc9d`):
//! `BtDriver::<BtClassic>::new(modem, Some(nvs))`, `EspGap::new`,
//! `set_device_name`, `EspSpp::new` with `SppConfig { mode: Cb, .. }`,
//! `subscribe`, `start_server(Security::Authenticate, Role::Slave, 0,
//! name)`, `SppEvent::{Start, ServerOpen, Close, Cong, DataInd, Write}`,
//! plus the SSP/legacy-pairing replies (`esp_bt_gap_ssp_confirm_reply`,
//! `esp_bt_gap_set_security_param(ESP_BT_SP_IOCAP_MODE)`,
//! `EspGap::request_variable_pin`). The required sdkconfig symbols are the
//! example's documented list (classic-only Bluedroid + SPP +
//! `CONFIG_BTDM_CTRL_MODE_BR_EDR_ONLY`), carried here by
//! `sdkconfig-xtensa-spp.full`.
//!
//! # Clean-wire notes
//!
//! The CCID wire is the RFCOMM stream: only bytes produced by
//! [`CcidSerialServer`] may reach `esp_spp_write`. Logs go to UART0 (the
//! console is free in `spp` builds — the FTDI bridge no longer carries
//! CCID). Each new connection starts with a purged ring and a reset
//! parser (`SppLink::on_open`/`on_close` clear the ring; the shell resets
//! the parser on connection close so the next host gets a clean SYNC).

use crate::ccid_handler::CcidHandler;
use crate::ccid_serial_server::{CcidSerialServer, ServeConfig};
use crate::nfc::NfcDriver;

/// One received byte plus the tick it arrived at. The tick rides with the
/// byte so the serving core's inter-byte stall guard
/// (`CcidSerialServer::INTER_BYTE_STALL_TICKS`) sees REAL arrival gaps even
/// though the shell drains the ring in batches — identical semantics to the
/// UART main reading byte-by-byte with a fresh `xTaskGetTickCount()` per
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxByte {
    pub byte: u8,
    pub tick: u32,
}

/// RX ring entries: sized for one full GemPC command frame (274 B) plus
/// RFCOMM segmentation slack. Overflow drops the WHOLE chunk (a partial
/// chunk would corrupt framing) and flags a resync.
pub const SPP_RX_RING_ENTRIES: usize = 512;

/// TX packet cap: one complete exchange (echo ≤ 274 + notify 2 +
/// response ≤ 275 = 551 B) with headroom. A well-behaved libccidtwin host
/// never pipelines two commands, so this cannot overflow in practice; a
/// hostile one just desyncs itself (dropped-response counter says so).
pub const SPP_TX_BUF_SIZE: usize = 1024;

/// Bounded RX ring (callback → serve loop). FIFO; whole-chunk drop on
/// overflow with a sticky flag the loop converts into a parser reset.
pub struct SppRxRing {
    entries: [RxByte; SPP_RX_RING_ENTRIES],
    head: usize,
    count: usize,
    overflow: bool,
    dropped: u32,
}

impl SppRxRing {
    pub fn new() -> Self {
        Self {
            entries: [RxByte { byte: 0, tick: 0 }; SPP_RX_RING_ENTRIES],
            head: 0,
            count: 0,
            overflow: false,
            dropped: 0,
        }
    }

    /// Push one received chunk (an SPP `DataInd`). All bytes share the
    /// arrival tick (they arrived in one RFCOMM packet).
    pub fn push(&mut self, bytes: &[u8], tick: u32) {
        if bytes.len() > SPP_RX_RING_ENTRIES - self.count {
            // Drop the whole chunk: partial admission would hand the parser
            // a truncated frame prefix that eats the NEXT valid frame.
            self.overflow = true;
            self.dropped = self.dropped.saturating_add(bytes.len() as u32);
            return;
        }
        for &b in bytes {
            self.entries[self.head] = RxByte { byte: b, tick };
            self.head = (self.head + 1) % SPP_RX_RING_ENTRIES;
            self.count += 1;
        }
    }

    /// Pop up to `out.len()` entries (short critical section: the shell
    /// copies out under the shared lock and feeds the server unlocked).
    pub fn pop_batch(&mut self, out: &mut [RxByte]) -> usize {
        let mut n = 0;
        while n < out.len() && self.count > 0 {
            let tail = (self.head + SPP_RX_RING_ENTRIES - self.count) % SPP_RX_RING_ENTRIES;
            out[n] = self.entries[tail];
            self.count -= 1;
            n += 1;
        }
        n
    }

    /// Connection lifecycle reset (clean-wire: a new host starts from a
    /// clean SYNC, not from the previous host's partial frame). Cumulative
    /// drop counters survive for diagnostics.
    pub fn clear(&mut self) {
        self.head = 0;
        self.count = 0;
        self.overflow = false;
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn len(&self) -> usize {
        self.count
    }

    /// Sticky overflow flag: true at most once per overflow event — the
    /// caller answers with a parser reset (read-idle path).
    pub fn take_overflow(&mut self) -> bool {
        core::mem::replace(&mut self.overflow, false)
    }

    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

/// Connection state shared between the SPP callback and the serve loop.
/// All transitions are cheap (no I/O) so the callback never blocks the
/// Bluedroid task on card business.
pub struct SppLink {
    handle: Option<u32>,
    congested: bool,
    last_rx_tick: u32,
    pending: [u8; SPP_TX_BUF_SIZE],
    pending_len: usize,
    /// #153: absolute tick by which the parked bytes must reach the wire
    /// (the producing response's `frame_done + response window`). `None`
    /// = undated (NAK-only packets — not governed by the host read
    /// window).
    pending_deadline: Option<u32>,
    dropped_tx: u32,
}

impl SppLink {
    pub fn new(now_ticks: u32) -> Self {
        Self {
            handle: None,
            congested: false,
            last_rx_tick: now_ticks,
            pending: [0; SPP_TX_BUF_SIZE],
            pending_len: 0,
            pending_deadline: None,
            dropped_tx: 0,
        }
    }

    /// `ESP_SPP_SRV_OPEN_EVT`: a client connected. Fresh protocol state —
    /// parked bytes from a dead connection are garbage now.
    pub fn on_open(&mut self, handle: u32, now_ticks: u32) {
        self.handle = Some(handle);
        self.congested = false;
        if self.pending_len > 0 {
            self.dropped_tx = self.dropped_tx.saturating_add(self.pending_len as u32);
        }
        self.pending_len = 0;
        self.pending_deadline = None;
        self.last_rx_tick = now_ticks;
    }

    /// `ESP_SPP_CLOSE_EVT`: connection gone. Undelivered pending bytes
    /// are counted, not silently kept.
    pub fn on_close(&mut self, now_ticks: u32) {
        if self.pending_len > 0 {
            self.dropped_tx = self.dropped_tx.saturating_add(self.pending_len as u32);
        }
        self.handle = None;
        self.congested = false;
        self.pending_len = 0;
        self.pending_deadline = None;
        self.last_rx_tick = now_ticks;
    }

    /// `ESP_SPP_CONG_EVT` (and the `cong` flag riding `SppEvent::Write`).
    pub fn on_cong(&mut self, congested: bool) {
        self.congested = congested;
    }

    /// Stamp a received chunk (drives the read-idle emulation).
    pub fn on_rx(&mut self, now_ticks: u32) {
        self.last_rx_tick = now_ticks;
    }

    pub fn handle(&self) -> Option<u32> {
        self.handle
    }

    pub fn congested(&self) -> bool {
        self.congested
    }

    pub fn last_rx_tick(&self) -> u32 {
        self.last_rx_tick
    }

    /// ms since the last received byte (wrapping-safe; huge when idle).
    pub fn rx_idle_ms(&self, now_ticks: u32) -> u32 {
        now_ticks.wrapping_sub(self.last_rx_tick)
    }

    /// Park a packet that could not be written (congestion or a failed
    /// `esp_spp_write`). Strictly single-slot: the protocol is
    /// request/response, so a second park means the host desynced — the
    /// older bytes are dropped and counted, order among DELIVERED bytes
    /// is never reordered. `deadline_tick` is the packet's #153 expiry
    /// ([`SppPump::tx_deadline`]); the retry path drops expired bytes
    /// instead of writing them.
    pub fn park(&mut self, bytes: &[u8], deadline_tick: Option<u32>) {
        let n = bytes.len().min(SPP_TX_BUF_SIZE);
        if self.pending_len > 0 {
            self.dropped_tx = self.dropped_tx.saturating_add(self.pending_len as u32);
        }
        self.pending[..n].copy_from_slice(&bytes[..n]);
        self.pending_len = n;
        self.pending_deadline = deadline_tick;
        if bytes.len() > SPP_TX_BUF_SIZE {
            self.dropped_tx = self.dropped_tx.saturating_add((bytes.len() - n) as u32);
        }
    }

    pub fn pending(&self) -> &[u8] {
        &self.pending[..self.pending_len]
    }

    pub fn clear_pending(&mut self) {
        self.pending_len = 0;
        self.pending_deadline = None;
    }

    /// #153 for parked bytes: has the host read window that was open
    /// when these bytes were produced closed while they sat parked? A
    /// late write is the same stream poison as a late response — the
    /// retry must drop them (Codex review on #158, P2). Wraparound-safe
    /// absolute-deadline form of the serve core's
    /// `now - frame_done > window`: elapsed-since-deadline is small once
    /// strictly past it, huge (2^32 − δ) before it.
    pub fn pending_expired(&self, now_ticks: u32) -> bool {
        match (self.pending_deadline, self.pending_len > 0) {
            (Some(deadline), true) => {
                let since = now_ticks.wrapping_sub(deadline);
                since != 0 && since < 0x8000_0000
            }
            _ => false,
        }
    }

    /// Count TX bytes dropped because no connection exists to carry them.
    pub fn note_dropped_tx(&mut self, bytes: usize) {
        self.dropped_tx = self.dropped_tx.saturating_add(bytes as u32);
    }

    pub fn dropped_tx(&self) -> u32 {
        self.dropped_tx
    }
}

/// What to do with freshly produced TX bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deliver {
    /// Hand the packet to `esp_spp_write`.
    Write,
    /// Park it in [`SppLink`] (congested, or older bytes are still parked —
    /// never write past a backlog).
    Park,
    /// No connection: drop and count.
    Drop,
}

pub fn delivery_plan(link: &SppLink) -> Deliver {
    if link.handle.is_none() {
        Deliver::Drop
    } else if link.congested || link.pending_len > 0 {
        Deliver::Park
    } else {
        Deliver::Write
    }
}

/// Serving outcome of one pumped batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PumpOutcome {
    pub responded: u32,
    pub naks: u32,
    /// Responses dropped for exceeding the host read window (#153) —
    /// produced no TX bytes.
    pub late_dropped: u32,
}

impl PumpOutcome {
    pub fn any(&self) -> bool {
        self.responded != 0 || self.naks != 0
    }

    pub fn merge(&mut self, other: PumpOutcome) {
        self.responded += other.responded;
        self.naks += other.naks;
        self.late_dropped += other.late_dropped;
    }
}

/// The SPP serving pump: [`CcidSerialServer`] with the UART-main wire
/// profile plus a TX packetizer. Drives the exact `serve_uart` cadence:
///
/// - per byte: `feed_byte(byte, arrival_tick)` (real arrival ticks keep
///   the inter-byte stall guard honest across RFCOMM chunking);
/// - `Respond`: append echo → notify → response to the TX packet (one
///   `esp_spp_write`; the GemPC host parser is a byte stream, so packet
///   boundaries carry no meaning — only ORDER does);
/// - `Nak`: append the 3-byte NAK frame;
/// - after delivery: `record_turnaround` → `poll_if_due` → snapshot
///   check → `poll_if_due_after_response` (shell side, same order as
///   `serve_uart`'s Respond arm).
pub struct SppPump<D: NfcDriver> {
    server: CcidSerialServer<D>,
    tx: [u8; SPP_TX_BUF_SIZE],
    tx_len: usize,
    /// #153 deadline of the OLDEST bytes in `tx` (first response
    /// appended since the last [`Self::tx_clear`]) — `None` while the
    /// packet holds no dated response (empty, or NAK-only).
    tx_deadline: Option<u32>,
    respond_stamp_us: Option<i64>,
    dropped_responses: u32,
}

impl<D: NfcDriver> SppPump<D> {
    /// m5stick UART-main profile, mirroring `serve_uart` in `main.rs`:
    /// NAK on malformed frames, NotifySlotChange on transitions, no
    /// inline polls (#89 — a physical poll must never delay a pending
    /// response; with SPP "pending" includes parked-but-undelivered).
    pub fn new(
        handler: CcidHandler<D>,
        poll_interval_ticks: u32,
        response_deadline_ticks: u32,
        now_ticks: u32,
    ) -> Self {
        Self {
            server: CcidSerialServer::with_config(
                handler,
                poll_interval_ticks,
                response_deadline_ticks,
                now_ticks,
                ServeConfig {
                    nak_on_error: true,
                    notify_slot_change: true,
                    inline_poll: false,
                },
            ),
            tx: [0; SPP_TX_BUF_SIZE],
            tx_len: 0,
            tx_deadline: None,
            respond_stamp_us: None,
            dropped_responses: 0,
        }
    }

    /// Feed one drained batch through the server. Appends the wire output
    /// (echo/notify/response triple, or NAK) to the TX packet. All-or-
    /// nothing per frame: a packet that would not fit whole is dropped
    /// and counted — a truncated frame on the stream is worse than a
    /// dropped one. `now` is the live tick source (#153
    /// answer-or-silence): it is consulted AFTER each `feed_byte` — the
    /// card handler runs synchronously inside it, and its elapsed time
    /// must count against the host read window (Codex review on #158,
    /// P1). Expired responses are dropped, never appended.
    pub fn pump_rx<F: Fn() -> u32>(&mut self, entries: &[RxByte], now: F) -> PumpOutcome {
        use crate::ccid_serial_server::ServeAction;
        let mut outcome = PumpOutcome::default();
        for RxByte { byte, tick } in entries {
            match self.server.feed_byte(*byte, *tick) {
                ServeAction::Respond => {
                    if self.server.response_expired(now()) {
                        self.server.record_late_response();
                        outcome.late_dropped += 1;
                        continue;
                    }
                    let start = self.tx_len;
                    let ok = append_slice(&mut self.tx, &mut self.tx_len, self.server.echo())
                        && append_slice(&mut self.tx, &mut self.tx_len, self.server.notify())
                        && append_slice(&mut self.tx, &mut self.tx_len, self.server.response());
                    if ok {
                        // Oldest bytes govern the parked-retry deadline.
                        self.tx_deadline
                            .get_or_insert(self.server.response_deadline_tick());
                        self.respond_stamp_us = Some(crate::t1::now_us());
                        outcome.responded += 1;
                    } else {
                        // Roll back — never emit a partial frame.
                        self.tx_len = start;
                        if self.tx_len == 0 {
                            self.tx_deadline = None;
                        }
                        self.dropped_responses += 1;
                    }
                }
                ServeAction::Nak => {
                    let start = self.tx_len;
                    if append_slice(&mut self.tx, &mut self.tx_len, self.server.nak_frame()) {
                        outcome.naks += 1;
                    } else {
                        self.tx_len = start;
                        self.dropped_responses += 1;
                    }
                }
                ServeAction::None => {}
            }
        }
        outcome
    }

    /// TX packet accumulated by [`Self::pump_rx`] (empty when nothing to
    /// deliver).
    pub fn tx(&self) -> &[u8] {
        &self.tx[..self.tx_len]
    }

    /// Mutable view for `EspSpp::write` (its C signature demands `*mut`).
    pub fn tx_packet_mut(&mut self) -> &mut [u8] {
        &mut self.tx[..self.tx_len]
    }

    /// Drop the delivered packet (after a successful write or a park).
    pub fn tx_clear(&mut self) {
        self.tx_len = 0;
        self.tx_deadline = None;
    }

    /// #153 deadline carried by the TX packet — the absolute tick by
    /// which the OLDEST bytes in it must reach the wire. Parking call
    /// sites hand this to [`SppLink::park`] so the retry path can drop
    /// expired bytes instead of writing them late. `None` for NAK-only
    /// packets (the read window guards responses).
    pub fn tx_deadline(&self) -> Option<u32> {
        self.tx_deadline
    }

    /// Issue #120 turnaround stamp, measured from the completing byte to
    /// the accepted write — the SPP analogue of `serve_uart`'s
    /// last-byte-read → flush_write span (`esp_spp_write` is queued, not
    /// flushed, so this is a lower bound on the wire-visible turnaround).
    pub fn record_turnaround(&mut self) {
        if let Some(stamp) = self.respond_stamp_us.take() {
            let us = (crate::t1::now_us() - stamp).clamp(0, u32::MAX as i64) as u32;
            self.server.handler_mut().record_turnaround_us(us);
        }
    }

    /// Read-idle path (no bytes for `SPP_IDLE_TIMEOUT_MS`): abandon any
    /// partial frame, poll card presence if due. Returns `true` when a
    /// poll ran. Also the overflow-resync path.
    pub fn idle(&mut self, now_ticks: u32) -> bool {
        self.server.on_read_idle(now_ticks)
    }

    pub fn poll_if_due(&mut self, now_ticks: u32) -> bool {
        self.server.poll_if_due(now_ticks)
    }

    /// Codex PR 98: keeps presence fresh under continuous traffic (the
    /// SPP loop's 10 ms cadence replaces the UART read-timeout wakeup).
    pub fn poll_if_due_after_response(&mut self, now_ticks: u32) -> bool {
        self.server.poll_if_due_after_response(now_ticks)
    }

    /// Escape 0xD1 passthrough — the shell panics after the ack is handed
    /// to the stack so the flash coredump captures live state
    /// (dump-and-retrieve; AGENTS.md).
    pub fn take_snapshot_request(&mut self) -> bool {
        self.server.take_snapshot_request()
    }

    pub fn handler_mut(&mut self) -> &mut CcidHandler<D> {
        self.server.handler_mut()
    }

    pub fn dropped_responses(&self) -> u32 {
        self.dropped_responses
    }
}

fn append_slice(buf: &mut [u8], len: &mut usize, bytes: &[u8]) -> bool {
    if *len + bytes.len() > buf.len() {
        return false;
    }
    buf[*len..*len + bytes.len()].copy_from_slice(bytes);
    *len += bytes.len();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccid_serial_server::response_window_ticks;
    use crate::nfc::MockNfcDriver;
    use ccid_protocol::types::PC_TO_RDR_GET_SLOT_STATUS;
    use ccid_transport_serial::{calculate_lrc, CTRL_ACK, SYNC};

    const POLL_INTERVAL: u32 = 500;

    /// Build a host→reader command frame: SYNC + ACK + 10-byte CCID header
    /// (dwLength = 0) + LRC (same helper shape as ccid_serial_server tests).
    fn command_frame(message_type: u8, seq: u8) -> [u8; 13] {
        let ccid = [message_type, 0, 0, 0, 0, 0, seq, 0, 0, 0];
        let mut frame = [0u8; 13];
        frame[0] = SYNC;
        frame[1] = CTRL_ACK;
        frame[2..12].copy_from_slice(&ccid);
        frame[12] = calculate_lrc(&frame[..12]);
        frame
    }

    fn pump_with(card_present: bool) -> SppPump<MockNfcDriver> {
        let mut driver = MockNfcDriver::new(card_present, &[0x3B, 0x80, 0x01, 0x01], &[0x90, 0x00]);
        driver.init().unwrap();
        // 1 kHz tick model (ticks == ms), like ccid_serial_server tests.
        SppPump::new(
            CcidHandler::new(driver),
            POLL_INTERVAL,
            response_window_ticks(1_000),
            0,
        )
    }

    fn push(ring: &mut SppRxRing, bytes: &[u8], tick: u32) {
        ring.push(bytes, tick);
    }

    fn drain_and_pump<D: NfcDriver>(
        ring: &mut SppRxRing,
        pump: &mut SppPump<D>,
        now: u32,
    ) -> PumpOutcome {
        let mut batch = [RxByte { byte: 0, tick: 0 }; SPP_RX_RING_ENTRIES];
        let mut outcome = PumpOutcome::default();
        let batch = &mut batch[..];
        loop {
            let n = ring.pop_batch(batch);
            if n == 0 {
                break;
            }
            outcome.merge(pump.pump_rx(&batch[..n], || now));
        }
        outcome
    }

    // ----- SppRxRing -----

    #[test]
    fn rx_ring_preserves_fifo_order() {
        let mut ring = SppRxRing::new();
        push(&mut ring, &[1, 2, 3], 10);
        push(&mut ring, &[4, 5], 12);
        let mut out = [RxByte { byte: 0, tick: 0 }; 8];
        let n = ring.pop_batch(&mut out);
        assert_eq!(n, 5);
        let bytes: Vec<u8> = out[..n].iter().map(|e| e.byte).collect();
        assert_eq!(bytes, vec![1, 2, 3, 4, 5]);
        // Ticks ride with their chunk.
        assert!(out[..3].iter().all(|e| e.tick == 10));
        assert!(out[3..5].iter().all(|e| e.tick == 12));
    }

    #[test]
    fn rx_ring_wraps_around_capacity() {
        let mut ring = SppRxRing::new();
        // Fill + drain in alternating phases totalling well past capacity.
        let mut sent = 0u16;
        for _ in 0..8 {
            let chunk: Vec<u8> = (0..64).map(|i| ((sent + i) & 0xFF) as u8).collect();
            sent += 64;
            push(&mut ring, &chunk, 0);
            let mut out = [RxByte { byte: 0, tick: 0 }; 64];
            assert_eq!(ring.pop_batch(&mut out), 64);
            let got: Vec<u8> = out.iter().map(|e| e.byte).collect();
            assert_eq!(got, chunk);
        }
    }

    #[test]
    fn rx_ring_overflow_drops_whole_chunk_and_flags() {
        let mut ring = SppRxRing::new();
        let fill = vec![0xAA; SPP_RX_RING_ENTRIES - 4];
        push(&mut ring, &fill, 0);
        assert!(!ring.take_overflow());
        // Does not fit in the remaining 4 slots: dropped whole, none of it
        // admitted (a partial prefix would corrupt framing).
        push(&mut ring, &[1, 2, 3, 4, 5], 1);
        assert!(ring.take_overflow());
        assert!(!ring.take_overflow(), "overflow flag is sticky-once");
        assert_eq!(ring.dropped(), 5);
        assert_eq!(ring.len(), SPP_RX_RING_ENTRIES - 4);
        let mut out = [RxByte { byte: 0, tick: 0 }; SPP_RX_RING_ENTRIES];
        let n = ring.pop_batch(&mut out);
        assert_eq!(n, SPP_RX_RING_ENTRIES - 4);
        assert!(out[..n].iter().all(|e| e.byte == 0xAA));
    }

    #[test]
    fn rx_ring_clear_is_the_connection_purge() {
        let mut ring = SppRxRing::new();
        push(&mut ring, &[9, 9, 9], 0);
        ring.clear();
        assert!(ring.is_empty());
        assert!(
            !ring.take_overflow(),
            "clear also clears a pending overflow"
        );
        push(&mut ring, &[7], 5);
        let mut out = [RxByte { byte: 0, tick: 0 }; 4];
        assert_eq!(ring.pop_batch(&mut out), 1);
        assert_eq!(out[0].byte, 7);
    }

    // ----- SppLink -----

    #[test]
    fn link_lifecycle_and_congestion() {
        let mut link = SppLink::new(100);
        assert_eq!(link.handle(), None);
        assert_eq!(delivery_plan(&link), Deliver::Drop, "no connection: drop");

        link.on_open(7, 200);
        assert_eq!(link.handle(), Some(7));
        assert_eq!(delivery_plan(&link), Deliver::Write);
        assert_eq!(link.rx_idle_ms(205), 5);

        link.on_rx(210);
        assert_eq!(link.rx_idle_ms(210 + 400), 400);

        link.on_cong(true);
        assert_eq!(delivery_plan(&link), Deliver::Park, "congested: park");

        link.park(&[1, 2, 3], None);
        assert_eq!(link.pending(), &[1, 2, 3]);
        link.on_cong(false);
        assert_eq!(
            delivery_plan(&link),
            Deliver::Park,
            "backlog keeps parking even after congestion clears"
        );
        link.clear_pending();
        assert_eq!(delivery_plan(&link), Deliver::Write);

        link.park(&[4, 5], None);
        link.on_close(300);
        assert_eq!(link.handle(), None);
        assert_eq!(link.pending().len(), 0, "close discards undelivered bytes");
        assert_eq!(link.dropped_tx(), 2, "…but counts them");
    }

    #[test]
    fn link_park_is_single_slot_and_counts_drops() {
        let mut link = SppLink::new(0);
        link.on_open(1, 0);
        link.park(&[1, 2, 3, 4], Some(100));
        link.park(&[5, 6], None);
        assert_eq!(link.pending(), &[5, 6], "newest parked bytes win");
        assert_eq!(link.dropped_tx(), 4, "the overwritten slot is counted");
    }

    // ----- SppPump: the serve_uart contract over chunked SPP delivery -----

    #[test]
    fn pump_frame_split_across_rfcomm_chunks_answers_once() {
        // A 13-byte command frame arrives as 3 + 10 (two RFCOMM packets,
        // 4 ms apart): both chunks share the pump, the response is ONE
        // packet with echo before response.
        let mut ring = SppRxRing::new();
        let mut pump = pump_with(false);
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 7);

        push(&mut ring, &frame[..3], 1000);
        push(&mut ring, &frame[3..], 1004);

        let outcome = drain_and_pump(&mut ring, &mut pump, 1010);
        assert_eq!(outcome.responded, 1);
        assert_eq!(outcome.naks, 0);

        let tx = pump.tx();
        assert_eq!(&tx[..13], &frame, "echo leads the packet verbatim");
        assert_eq!(tx[13], SYNC, "framed response follows");
        assert_eq!(tx[14], CTRL_ACK);
        assert_eq!(tx[15], 0x81, "RDR_to_PC_SlotStatus");
        assert_eq!(tx[21], 7, "bSeq echoed");
        let n = tx.len();
        assert_eq!(tx[n - 1], calculate_lrc(&tx[..n - 1]), "response LRC valid");
    }

    #[test]
    fn pump_stall_gap_between_chunks_drops_the_frame_like_uart() {
        // The inter-byte stall guard (10 ticks) uses ARRIVAL ticks: a chunk
        // gap > 10 ms mid-frame means the frame began with garbage — the
        // parser resets, the frame is lost, exactly like the UART main.
        let mut ring = SppRxRing::new();
        let mut pump = pump_with(false);
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);

        push(&mut ring, &frame[..3], 1000);
        push(&mut ring, &frame[3..], 1100); // 100-tick gap: stall

        let outcome = drain_and_pump(&mut ring, &mut pump, 1110);
        assert_eq!(outcome.responded, 0);
        assert_eq!(pump.tx().len(), 0);
    }

    #[test]
    fn pump_naks_malformed_frames() {
        let mut ring = SppRxRing::new();
        let mut pump = pump_with(false);
        let mut bad = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        bad[12] ^= 0xFF; // break the LRC

        push(&mut ring, &bad, 0);
        let outcome = drain_and_pump(&mut ring, &mut pump, 10);
        assert_eq!(outcome.naks, 1);
        assert_eq!(pump.tx(), &[0x03, 0x15, 0x16], "the 3-byte NAK frame");
    }

    #[test]
    fn pump_two_frames_in_one_batch_stream_into_one_packet() {
        // libccidtwin never pipelines, but the byte stream does not care:
        // two exchanges in one batch produce one correctly ordered packet:
        // echo1 | response1 | echo2 | response2 (26 B per exchange).
        let mut ring = SppRxRing::new();
        let mut pump = pump_with(false);
        let f1 = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        let f2 = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 2);

        push(&mut ring, &f1, 0);
        push(&mut ring, &f2, 1);
        let outcome = drain_and_pump(&mut ring, &mut pump, 10);
        assert_eq!(outcome.responded, 2);

        let tx = pump.tx();
        assert_eq!(tx.len(), 52, "13+13 per exchange");
        assert_eq!(&tx[..13], &f1, "first echo");
        assert_eq!(tx[13 + 2], 0x81, "first response: SlotStatus");
        assert_eq!(tx[13 + 8], 1, "first response bSeq");
        assert_eq!(&tx[26..39], &f2, "second echo");
        assert_eq!(tx[39 + 2], 0x81, "second response: SlotStatus");
        assert_eq!(tx[39 + 8], 2, "second response bSeq");
    }

    #[test]
    fn pump_packet_cap_never_emits_partial_frames() {
        // Hostile pipelining beyond the packet cap: whole frames only.
        // The ring (512 entries) is drained in cycles so all 80 frames get
        // through; the TX packet is deliberately never cleared between
        // cycles — that accumulation is the overflow scenario.
        let mut ring = SppRxRing::new();
        let mut pump = pump_with(false);
        let mut total = PumpOutcome::default();
        for seq in 0..80u8 {
            let f = command_frame(PC_TO_RDR_GET_SLOT_STATUS, seq);
            push(&mut ring, &f, seq as u32);
            if seq % 10 == 9 {
                total.merge(drain_and_pump(&mut ring, &mut pump, 100));
            }
        }
        total.merge(drain_and_pump(&mut ring, &mut pump, 100));
        let dropped = pump.dropped_responses();
        assert!(dropped > 0, "cap must be hit with 80 pipelined frames");
        assert_eq!(
            total.responded as usize + dropped as usize,
            80,
            "every frame is either answered whole or counted"
        );
        // Whole-exchange invariant for these uniform 26-byte exchanges:
        // the packet is a multiple of 26 — no partial frame ever emitted.
        assert_eq!(pump.tx().len() % 26, 0);
        assert!(pump.tx().len() <= SPP_TX_BUF_SIZE);
    }

    #[test]
    fn pump_idle_polls_and_keeps_transitions_for_the_next_command_window() {
        // m5stick profile (#89/#98): no inline polls; idle polls stash a
        // transition that the NEXT GetSlotStatus delivers between echo and
        // response.
        let mut ring = SppRxRing::new();
        let mut pump = pump_with(false);
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);

        // Prime: an idle poll past the interval with no card.
        assert!(pump.idle(POLL_INTERVAL + 1));

        // Card lands on the coil; the next idle poll sees the transition.
        pump.handler_mut().driver_mut().set_card_present(true);
        assert!(pump.idle(2 * POLL_INTERVAL + 1));

        // Next command: notify rides between echo and response.
        push(&mut ring, &frame, 2 * POLL_INTERVAL + 20);
        let outcome = drain_and_pump(&mut ring, &mut pump, 2 * POLL_INTERVAL + 30);
        assert_eq!(outcome.responded, 1);
        let tx = pump.tx();
        assert_eq!(&tx[..13], &frame, "echo first");
        assert_eq!(&tx[13..15], &[0x50, 0x03], "NotifySlotChange present");
        assert_eq!(tx[15], SYNC, "framed response last");
    }

    #[test]
    fn pump_post_response_poll_under_continuous_traffic() {
        // Codex PR 98: with inline_poll=false the post-response poll is
        // what keeps presence fresh when commands never pause.
        let mut ring = SppRxRing::new();
        let mut pump = pump_with(true);
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        let polls_before = pump.handler_mut().driver_mut().poll_count();

        push(&mut ring, &frame, POLL_INTERVAL * 2 + 1);
        let outcome = drain_and_pump(&mut ring, &mut pump, POLL_INTERVAL * 2 + 10);
        assert_eq!(outcome.responded, 1);
        assert!(
            pump.poll_if_due_after_response(POLL_INTERVAL * 2 + 2),
            "post-response poll fires"
        );
        assert_eq!(
            pump.handler_mut().driver_mut().poll_count(),
            polls_before + 1
        );
    }

    #[test]
    fn pump_congestion_park_then_deliver_preserves_order() {
        // Full shell-shaped flow: respond while congested → park; the
        // congestion clears; the SAME bytes are delivered unchanged.
        let mut ring = SppRxRing::new();
        let mut pump = pump_with(false);
        let mut link = SppLink::new(0);
        link.on_open(3, 0);

        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 9);
        push(&mut ring, &frame, 5);
        let outcome = drain_and_pump(&mut ring, &mut pump, 10);
        assert_eq!(outcome.responded, 1);
        let expected = pump.tx().to_vec();

        link.on_cong(true);
        assert_eq!(delivery_plan(&link), Deliver::Park);
        let parked_deadline = pump.tx_deadline();
        link.park(pump.tx(), parked_deadline);
        pump.tx_clear();
        assert_eq!(link.pending(), &expected[..]);

        link.on_cong(false);
        // Pending retry: delivered verbatim, before any new traffic.
        assert_eq!(link.pending(), &expected[..]);
        // The parked deadline is the response's own (frame done at tick
        // 5 + 2500-tick window): still inside it at 2504/2505, past it
        // at 2506 — a retry then must drop, not write.
        assert_eq!(parked_deadline, Some(2_505));
        assert!(!link.pending_expired(2_504));
        assert!(!link.pending_expired(2_505));
        assert!(link.pending_expired(2_506));
        link.clear_pending();
    }

    #[test]
    fn pump_drops_response_past_host_deadline() {
        // #153 answer-or-silence at the SPP write site: the batch is
        // pumped long after the frame completed (host read window long
        // closed) — the exchange produces NO TX bytes, and the next
        // fresh frame serves normally.
        let mut ring = SppRxRing::new();
        let mut pump = pump_with(false);
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 3);
        push(&mut ring, &frame, 1_000);

        let outcome = drain_and_pump(&mut ring, &mut pump, 1_000 + 2_600);
        assert_eq!(outcome.responded, 0);
        assert_eq!(outcome.late_dropped, 1);
        assert_eq!(pump.tx().len(), 0, "silence — no late bytes on the wire");
        assert_eq!(pump.server.late_drop_count(), 1);

        let next = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 4);
        push(&mut ring, &next, 1_000 + 2_700);
        let outcome = drain_and_pump(&mut ring, &mut pump, 1_000 + 2_710);
        assert_eq!(outcome.responded, 1);
        assert_eq!(&pump.tx()[..13], &next, "echo of the fresh frame");
    }

    #[test]
    fn pump_expiry_samples_the_clock_per_response() {
        // Codex #158 P1: the #153 tick must be consulted AFTER feed_byte
        // ran the card handler, PER Respond. Two frames in one batch, and
        // a clock that advances one full window per consultation: the
        // first response is judged fresh, the second (whose check sees
        // the advanced clock) is late — a single frozen pre-handler
        // sample would return the same verdict for both.
        let mut ring = SppRxRing::new();
        let mut pump = pump_with(false);
        let f1 = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        let f2 = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 2);
        push(&mut ring, &f1, 1_000);
        // 5-tick gap: inside the inter-byte stall guard, so f2 parses as
        // a real second frame (13 ticks would trip the guard).
        push(&mut ring, &f2, 1_005);

        let clock = std::cell::Cell::new(1_020u32);
        let mut batch = [RxByte { byte: 0, tick: 0 }; SPP_RX_RING_ENTRIES];
        let n = ring.pop_batch(&mut batch);
        let outcome = pump.pump_rx(&batch[..n], || {
            let t = clock.get();
            clock.set(t + 2_600);
            t
        });

        assert_eq!(
            outcome.responded, 1,
            "first check: 20 ticks elapsed — fresh"
        );
        assert_eq!(
            outcome.late_dropped, 1,
            "second check: 3620 — past the window"
        );
        // First exchange's 26 bytes went out; the late one is silence.
        assert_eq!(pump.tx().len(), 26);
    }

    #[test]
    fn parked_bytes_expire_at_their_response_deadline() {
        let mut link = SppLink::new(0);
        link.on_open(1, 0);
        // Undated park (NAK-only packet): retry behavior unchanged.
        link.park(&[0x03, 0x15, 0x16], None);
        assert!(
            !link.pending_expired(u32::MAX - 1),
            "undated bytes never expire by #153"
        );
        link.clear_pending();

        link.park(&[1, 2, 3], Some(3_500));
        assert!(!link.pending_expired(3_499));
        assert!(!link.pending_expired(3_500), "at the deadline: in window");
        assert!(link.pending_expired(3_501), "past it: silence, never write");
        // Long-before reads as a huge wrapping delta, not as expired.
        assert!(!link.pending_expired(1_000));
    }

    #[test]
    fn parked_deadline_survives_tick_wraparound() {
        let mut link = SppLink::new(0);
        link.on_open(1, 0);
        let deadline = u32::MAX - 100;
        link.park(&[9, 9], Some(deadline));
        assert!(
            !link.pending_expired(deadline - 1),
            "before it, below the wrap"
        );
        assert!(!link.pending_expired(deadline), "at it");
        assert!(
            link.pending_expired(deadline + 1),
            "just past, above the wrap"
        );
    }

    #[test]
    fn pump_carries_the_oldest_response_deadline() {
        // Two exchanges appended to one packet: the parked deadline is
        // the FIRST exchange's (its bytes are the oldest — they expire
        // first), and tx_clear resets it.
        let mut ring = SppRxRing::new();
        let mut pump = pump_with(false);
        let f1 = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        let f2 = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 2);
        push(&mut ring, &f1, 1_000);
        push(&mut ring, &f2, 1_005);

        let outcome = drain_and_pump(&mut ring, &mut pump, 1_020);
        assert_eq!(outcome.responded, 2);
        assert_eq!(
            pump.tx_deadline(),
            Some(1_000 + 2_500),
            "first frame_done + window, not the second's"
        );
        pump.tx_clear();
        assert_eq!(pump.tx_deadline(), None);
    }

    #[test]
    fn pump_overflow_resyncs_and_next_clean_frame_is_served() {
        // Ring overflow (host sent garbage faster than we drain): the loop
        // converts the flag into idle() (parser reset); the next clean
        // frame answers normally.
        let mut ring = SppRxRing::new();
        let mut pump = pump_with(false);

        let flood = vec![0xEE; SPP_RX_RING_ENTRIES + 100];
        push(&mut ring, &flood, 0);
        assert!(ring.take_overflow());
        assert!(ring.dropped() >= 100);
        // Past the poll interval the idle path both resets the parser and
        // runs the presence poll; the next clean frame proves the resync.
        assert!(pump.idle(POLL_INTERVAL + 1));

        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 4);
        push(&mut ring, &frame, 50);
        let outcome = drain_and_pump(&mut ring, &mut pump, 60);
        assert_eq!(outcome.responded, 1);
        assert_eq!(&pump.tx()[..13], &frame);
    }
}

// ===========================================================================
// Target shell — `spp` feature only (the pure core above is always compiled
// and host-tested, mirroring the `ble_log_queue`/`ble_console` split).
//
// Compile-loud pairing (AGENTS.md rule, same as `ble`): the `spp` feature
// REQUIRES a classic-BT+SPP sdkconfig (`sdkconfig-xtensa-spp.full`). The
// `esp_idf_svc::bt::spp` module only exists under
// `cfg(all(esp32, esp_idf_bt_classic_enabled, esp_idf_bt_spp_enabled))`
// inside esp-idf-svc, so a mismatched pair fails to COMPILE at the imports
// below — deliberately loud. Classic BR/EDR is ESP32-silicon-only: there is
// no `c3-ccid` SPP build for the same reason there is no `c3-ccid-ble`.
// ===========================================================================
#[cfg(all(feature = "spp", any(target_arch = "xtensa", target_arch = "riscv32")))]
pub mod shell {
    use std::sync::{Arc, Mutex};

    use esp_idf_hal::delay::FreeRtos;
    use esp_idf_hal::modem::Modem;
    use esp_idf_svc::bt::gap::{DiscoveryMode, EspGap, GapEvent};
    use esp_idf_svc::bt::spp::{self, EspSpp, SppConfig, SppEvent, Status};
    use esp_idf_svc::bt::{reduce_bt_memory, BtClassic, BtDriver};
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use esp_idf_sys::{
        esp, esp_bt_gap_set_security_param, esp_bt_gap_ssp_confirm_reply,
        esp_bt_sp_param_t_ESP_BT_SP_IOCAP_MODE, ESP_BT_IO_CAP_IO,
    };

    use super::{delivery_plan, Deliver, PumpOutcome, RxByte, SppLink, SppPump, SppRxRing};
    use crate::ccid_handler::CcidHandler;
    use crate::ccid_serial_server::response_window_ticks;
    use crate::nfc::NfcDriver;

    /// Read-idle timeout — mirrors `UART_RX_TIMEOUT_MS` in the UART mains:
    /// this long without a byte means the parser resets and the interval
    /// poll fires.
    const SPP_IDLE_TIMEOUT_MS: u32 = 500;
    /// Card presence poll interval — mirrors `CARD_POLL_INTERVAL_MS`.
    const SPP_POLL_INTERVAL_MS: u32 = 3000;
    /// Serve-loop cadence: replaces the UART read-timeout wakeup; keeps the
    /// pending-retry and idle paths live without busy-spinning.
    const SPP_LOOP_DELAY_MS: u32 = 10;
    /// Escape 0xD1 ack-before-panic settle. The UART mains call
    /// `uart.flush_write()` between ack and panic; Cb-mode `esp_spp_write`
    /// has no flush — give the Bluedroid task a bounded window to drain the
    /// queued ack before the panic handler writes the coredump.
    const SPP_SNAPSHOT_SETTLE_MS: u32 = 100;
    /// RX drain granularity (entries copied out per lock hold).
    const SPP_POP_BATCH: usize = 128;

    /// GAP device name — matches the Escape 0x02 identity string
    /// ("GemPC Twin ESP32 1.0") so both identity probes agree.
    const DEVICE_NAME: &str = "GemPC Twin ESP32";
    /// SPP service name (SDP "Serial Port" record).
    const SERVICE_NAME: &str = "GemPC Twin SPP";

    /// Shared between the Bluedroid callback task (pushes) and the serve
    /// loop (pops). LOCK DISCIPLINE: the callback must never block on card
    /// I/O (#89 — MFRC522 polls run hundreds of ms), so the serve loop
    /// holds this mutex ONLY for memcpy-speed pops/pushes; everything slow
    /// (pump, card I/O, esp_spp_write) runs outside it.
    struct Shared {
        ring: SppRxRing,
        link: SppLink,
    }

    fn tick() -> u32 {
        unsafe { esp_idf_sys::xTaskGetTickCount() }
    }

    /// Bring up the SPP acceptor and serve GemPC CCID over RFCOMM.
    ///
    /// REFERENCE (Reference-First): every bring-up step below mirrors
    /// esp-rs/esp-idf-svc `examples/bt_spp_acceptor.rs` (crate 0.52.1,
    /// master c8bcc9d — byte-identical SPP/GAP API): BtDriver<BtClassic> +
    /// reduce_bt_memory, EspGap device name, EspSpp in Cb mode, subscribe
    /// before start_server (the Init event is consumed inside
    /// `EspSpp::new`), `Security::Authenticate` + `Role::Slave` + auto SCN,
    /// SSP auto-confirm + legacy-PIN replies, discoverable scan mode on
    /// `SppEvent::Start`.
    ///
    /// The serving cadence mirrors `serve_uart` in main.rs byte-for-byte:
    /// echo → notify → response, NAK on malformed frames, interval-gated
    /// presence polls, post-response polls (Codex PR 98), read-idle parser
    /// reset, Escape 0xD1 dump-and-retrieve.
    pub fn serve_spp<D: NfcDriver>(mut modem: Modem<'static>, handler: CcidHandler<D>) -> ! {
        // --- Bring-up (reference: bt_spp_acceptor.rs `main`) ---
        // `reduce_bt_memory`'s parameter is a zero-cost ownership marker;
        // this esp-idf-hal has no `reborrow` on it (the reference example
        // targets a newer hal) — `split_reborrow` yields the
        // BluetoothModem handle the marker wants, and the borrow ends
        // inside the call so `modem` still moves into BtDriver below.
        let nvs = EspDefaultNvsPartition::take().ok();
        let (_, bt_modem) = modem.split_reborrow();
        if let Err(e) = reduce_bt_memory(bt_modem) {
            log::warn!("spp: reduce_bt_memory: {e} (continuing)");
        }
        let bt = Arc::new(
            BtDriver::<BtClassic>::new(modem, nvs)
                .expect("spp: BtDriver init failed (sdkconfig-xtensa-spp.full?)"),
        );
        let gap = Arc::new(EspGap::new(bt.clone()).expect("spp: EspGap init failed"));
        gap.set_device_name(DEVICE_NAME)
            .expect("spp: set_device_name failed");
        let spp = Arc::new(
            EspSpp::new(
                bt.clone(),
                &SppConfig {
                    mode: spp::Mode::Cb,
                    enable_l2cap_ertm: true,
                    tx_buffer_size: 0, // Cb mode (reference example)
                },
            )
            .expect("spp: EspSpp init failed"),
        );

        let shared = Arc::new(Mutex::new(Shared {
            ring: SppRxRing::new(),
            link: SppLink::new(tick()),
        }));

        // --- GAP pairing handler (reference: `on_gap_event`) ---
        let gap_cb = gap.clone();
        gap.subscribe(move |event| match event {
            GapEvent::PairingPinRequest {
                bd_addr,
                min_16_digit,
            } => {
                // Legacy pairing: reply 1234 (16-digit zeros when the host
                // demands the long form) — reference example behavior.
                let pin: &[u8] = if min_16_digit {
                    &[0u8; 16]
                } else {
                    &[1, 2, 3, 4]
                };
                if let Err(e) = gap_cb.reply_variable_pin(&bd_addr, Some(pin)) {
                    log::warn!("spp: PIN reply failed: {e}");
                }
            }
            GapEvent::PairingUserConfirmationRequest { bd_addr, number } => {
                // SSP numeric compare: auto-accept on our side (the HOST
                // agent still shows the compare to the bench operator).
                log::info!("spp: SSP confirm {number} from {bd_addr} — accepting");
                if let Err(e) = esp!(unsafe {
                    esp_bt_gap_ssp_confirm_reply(&bd_addr as *const _ as *mut _, true)
                }) {
                    log::warn!("spp: SSP confirm failed: {e}");
                }
            }
            GapEvent::AuthenticationCompleted {
                bd_addr, status, ..
            } => {
                log::info!("spp: auth {bd_addr}: {status:?}");
            }
            GapEvent::AclDisconnected {
                bd_addr, handle, ..
            } => {
                log::info!("spp: ACL down {bd_addr} handle {handle}");
            }
            _ => {}
        })
        .expect("spp: GAP subscribe failed");

        // --- SPP event handler (reference: `on_spp_event`) ---
        let sh = shared.clone();
        let gap_spp = gap.clone();
        spp.subscribe(move |event| {
            let mut g = sh.lock().unwrap();
            match event {
                SppEvent::Start { status, scn, .. } => {
                    if status == Status::Success {
                        log::info!("spp: server started (scn {scn}) — discoverable");
                        if let Err(e) = gap_spp.set_scan_mode(true, DiscoveryMode::Discoverable) {
                            log::warn!("spp: set_scan_mode failed: {e}");
                        }
                    } else {
                        log::error!("spp: start failed: {status:?}");
                    }
                }
                SppEvent::ServerOpen {
                    status,
                    handle,
                    rem_bda,
                    ..
                } => {
                    if status == Status::Success {
                        log::info!("spp: client {rem_bda} connected (handle {handle})");
                        // Clean-wire per connection: purge whatever the
                        // previous host left mid-frame.
                        g.link.on_open(handle, tick());
                        g.ring.clear();
                    } else {
                        log::error!("spp: srv_open failed: {status:?}");
                    }
                }
                SppEvent::Close {
                    status,
                    handle,
                    async_,
                    ..
                } => {
                    log::info!("spp: handle {handle} closed ({status:?}, by-remote={async_})");
                    g.link.on_close(tick());
                    g.ring.clear();
                }
                SppEvent::Cong { cong, handle, .. } => {
                    log::debug!("spp: cong={cong} handle {handle}");
                    g.link.on_cong(cong);
                }
                SppEvent::DataInd {
                    status,
                    handle,
                    length,
                    data,
                } => {
                    if status == Status::Success {
                        // The data pointer is valid only inside this
                        // callback (reference example) — copy under the
                        // lock, never process here (#89).
                        let bytes = unsafe { core::slice::from_raw_parts(data, length as usize) };
                        let now = tick();
                        g.link.on_rx(now);
                        g.ring.push(bytes, now);
                    } else {
                        log::warn!("spp: data-ind {status:?} handle {handle}");
                    }
                }
                _ => {}
            }
        })
        .expect("spp: SPP subscribe failed");

        // --- Start the acceptor (reference: `create_service`) ---
        // SCN 0 = auto-allocate (the Start event reports it; BlueZ can
        // resolve it via `sdptool browse` — bench-observed default is 1).
        spp.start_server(
            spp::Security::Authenticate,
            spp::Role::Slave,
            0,
            SERVICE_NAME,
        )
        .expect("spp: start_server failed");
        // SSP IO capability: the esp-idf-svc helper is cfg-elided (the
        // CONFIG_BT_SSP_ENABLED sdkconfig symbol no longer exists in IDF
        // 5.x) — the reference example calls the raw API.
        esp!(unsafe {
            esp_bt_gap_set_security_param(
                esp_bt_sp_param_t_ESP_BT_SP_IOCAP_MODE,
                &ESP_BT_IO_CAP_IO as *const _ as *mut std::ffi::c_void,
                1,
            )
        })
        .expect("spp: set_security_param failed");
        // Legacy-pairing fallback: variable PIN, answered in the GAP
        // handler above (reference example).
        gap.request_variable_pin()
            .expect("spp: request_variable_pin failed");

        log::warn!(
            "FWID esp32-ccid rev={} build={} transport=spp device=\"{}\"",
            env!("FW_GIT_REV"),
            env!("FW_BUILD_TS"),
            DEVICE_NAME
        );

        // --- Serve loop: serve_uart's cadence over the SPP byte pipe ---
        let poll_interval_ticks =
            esp_idf_hal::delay::TickType::new_millis(SPP_POLL_INTERVAL_MS as u64).ticks() as u32;
        // #153 window in TICKS — tick-rate-derived (Codex #158 P1);
        // CONFIG_FREERTOS_HZ=100 here, so 2500 ms is 250 ticks.
        let response_deadline_ticks = response_window_ticks(esp_idf_sys::configTICK_RATE_HZ);
        let mut pump = SppPump::new(
            handler,
            poll_interval_ticks,
            response_deadline_ticks,
            tick(),
        );
        let mut batch = [RxByte { byte: 0, tick: 0 }; SPP_POP_BATCH];
        let mut retry_buf = [0u8; super::SPP_TX_BUF_SIZE];

        loop {
            let now = tick();

            // 1. RX: copy out under the lock, serve outside it (card I/O
            //    must never stall the Bluedroid callback). `tick` is
            //    passed as the live clock: pump_rx samples it AFTER the
            //    handler ran (#153 — handler time counts, Codex #158 P1).
            let mut outcome = PumpOutcome::default();
            loop {
                let n = {
                    let mut g = shared.lock().unwrap();
                    g.ring.pop_batch(&mut batch)
                };
                if n == 0 {
                    break;
                }
                outcome.merge(pump.pump_rx(&batch[..n], tick));
            }

            // 2. Deliver what the pump produced (echo → notify → response,
            //    or NAK) as ONE esp_spp_write.
            if outcome.any() {
                let (plan, handle) = {
                    let g = shared.lock().unwrap();
                    (delivery_plan(&g.link), g.link.handle())
                };
                let mut delivered = false;
                match plan {
                    Deliver::Write => {
                        let handle = handle.expect("delivery_plan checked for a handle");
                        match spp.write(handle, pump.tx_packet_mut()) {
                            Ok(()) => delivered = true,
                            Err(e) => {
                                // Busy/full: park and retry — the host is
                                // waiting for exactly these bytes (until
                                // their #153 deadline passes).
                                log::debug!("spp: write busy ({e}) — parking");
                                let deadline = pump.tx_deadline();
                                shared.lock().unwrap().link.park(pump.tx(), deadline);
                            }
                        }
                    }
                    Deliver::Park => {
                        let deadline = pump.tx_deadline();
                        shared.lock().unwrap().link.park(pump.tx(), deadline);
                    }
                    Deliver::Drop => {
                        shared.lock().unwrap().link.note_dropped_tx(pump.tx().len());
                    }
                }
                pump.tx_clear();

                // 3. Post-response cadence ONLY after delivery (#89: no
                //    physical poll may delay pending/parked bytes).
                if delivered {
                    pump.record_turnaround();
                    pump.poll_if_due(now);
                    // Dump-and-retrieve: ack is queued — settle, panic,
                    // coredump (AGENTS.md "ack-before-panic flush").
                    if pump.take_snapshot_request() {
                        FreeRtos::delay_ms(SPP_SNAPSHOT_SETTLE_MS);
                        panic!("escape 0xD1: diagnostic snapshot requested");
                    }
                    pump.poll_if_due_after_response(tick());
                }
            }

            // 4. Pending retry: congestion cleared / stack drained — but
            //    #153 still applies: bytes whose host read window closed
            //    while they sat parked are dropped and counted, never
            //    written late (Codex review on #158, P2).
            let retry: Option<(u32, usize, bool)> = {
                let g = shared.lock().unwrap();
                match g.link.handle() {
                    Some(h) if !g.link.congested() && !g.link.pending().is_empty() => {
                        let expired = g.link.pending_expired(tick());
                        let pending = g.link.pending();
                        retry_buf[..pending.len()].copy_from_slice(pending);
                        Some((h, pending.len(), expired))
                    }
                    _ => None,
                }
            };
            if let Some((handle, len, expired)) = retry {
                if expired {
                    let mut g = shared.lock().unwrap();
                    g.link.clear_pending();
                    g.link.note_dropped_tx(len);
                    log::debug!("spp: dropped {len} parked bytes past the host read window");
                } else if spp.write(handle, &mut retry_buf[..len]).is_ok() {
                    shared.lock().unwrap().link.clear_pending();
                }
            }

            // 5. Read-idle: no bytes for the timeout → parser reset +
            //    interval poll (mirrors the UART read-timeout arm). A ring
            //    overflow takes the same path (resync) plus diagnostics.
            let (idle_ms, overflow, dropped) = {
                let mut g = shared.lock().unwrap();
                (
                    g.link.rx_idle_ms(now),
                    g.ring.take_overflow(),
                    g.ring.dropped() + g.link.dropped_tx() + pump.dropped_responses(),
                )
            };
            if idle_ms >= SPP_IDLE_TIMEOUT_MS || overflow {
                pump.idle(now);
            }
            if overflow || dropped > 0 {
                log::warn!("spp: rx-overflow/tx-drops total {dropped}");
            }

            FreeRtos::delay_ms(SPP_LOOP_DELAY_MS);
        }
    }
}
