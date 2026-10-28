//! Host-testable CDC-class burst serve loop (issue #119).
//!
//! Extraction of the nucula USB-CDC main's burst loop
//! (`firmware/esp32-ccid/src/pn7160_ccid.rs`, commit 66109dd): the
//! loop logic lived inline in the firmware bin — untestable on host
//! and unduplicable for sibling CDC-class transports. It moves here
//! for the same reason the SPP pump did (`spp_server`): one place to
//! be right, mock-transport unit tests in CI.
//!
//! # Why bursts (the #119 latency story)
//!
//! The original loop called `usb.read(&mut [u8; 1], …)` one byte per
//! syscall: a 13-byte GetSlotStatus frame cost 13 driver round-trips.
//! 66109dd switched to a 64-byte buffer (one full-speed CDC bulk
//! packet) — `usb.read` returns whatever the driver ring holds up to
//! `buf.len()`, so a whole frame lands in ONE call (13→1 reads per
//! minimal frame, ~268→5 per max-APDU frame; bench 2026-10-11: syscall
//! reduction real, direct-serial rtt unchanged at p50 0.20 ms — the
//! "~300 ms/command" originally reported was a host-measurement
//! artifact, see the #119 issue comments before re-attributing it).
//!
//! # Transport contract (CDC-class, NOT the UART contract)
//!
//! This loop is for transports whose read returns AVAILABLE bytes (up
//! to `buf.len()`) after at most the timeout — the `usb_serial`
//! `xRingbufferReceiveUpTo` semantics. The classic UART drivers wait
//! for the FULL requested length, so a larger buffer there ADDS
//! latency; the UART mains keep their 1-byte-blocking-read + drain
//! shape in `main.rs` (`serve_uart`). Do not "unify" the two — the
//! read semantics differ, not just the wire.
//!
//! # Preserved on-device behavior (bench-verified, 66109dd)
//!
//! - per-byte `feed_byte` semantics unchanged (parser completes only
//!   on the frame's LRC byte, so a mid-burst `Respond` flushes BEFORE
//!   the remaining burst bytes — leftover bytes start the next host
//!   frame; multi-frame-in-one-USB-write keeps `echoA,respA,echoB,respB`
//!   ordering);
//! - #153/#168 arrival-floor discipline: bytes queued in the ring
//!   during a blocking handler are anchored at the tick of the last
//!   DISPATCH, never at the drain-time read; a SHORT read (n < buf
//!   len) plausibly drained the ring and raises the floor, a FULL read
//!   may leave older queued bytes behind and must keep it; an empty
//!   read proves the ring idle and raises the floor;
//! - answer-or-silence (#153): responses that outlived the host read
//!   window are dropped (`record_late_response`), never written;
//! - #294 WS2: OTA serve evidence fires only when echo (+notify) AND
//!   response actually reached the wire;
//! - escape 0xD1 dump-and-retrieve: after the ack response is flushed
//!   the loop STOPS consuming the burst and reports
//!   [`BurstOutcome::snapshot_requested`] — the caller panics
//!   immediately so the flash coredump captures live state with the
//!   ack already on the wire.

use crate::ccid_serial_server::{CcidSerialServer, ServeAction};
use crate::nfc::NfcDriver;
use card_interface::FrontendDiagnostics;

/// Read-failure marker for [`BurstWire::read_burst`] — the serve loop
/// treats it like an idle read (the on-device loop mapped every
/// non-positive read to the idle path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurstReadError;

/// CDC-class byte transport seam (issue #119): reads return a whole
/// burst of queued bytes (see the module docs for the read contract),
/// writes deliver one frame or fail.
///
/// The firmware bin implements this for its driver handle
/// (`UsbSerialDriver` on the nucula); host tests implement it with a
/// scripted mock — the "mock transports" the #119 fix was designed
/// for.
pub trait BurstWire {
    /// Read up to `buf.len()` bytes into `buf`, returning after at
    /// most `timeout_ticks` with whatever the ring holds.
    /// `Ok(0)` = no data within the timeout (read idle); `Err` is
    /// treated like idle.
    fn read_burst(&mut self, buf: &mut [u8], timeout_ticks: u32) -> Result<usize, BurstReadError>;

    /// Write one complete wire frame (echo, notify, response or NAK).
    /// Returns `false` on failure — the caller treats a failed frame
    /// write as a failed exchange (no OTA serve evidence).
    fn write_frame(&mut self, bytes: &[u8]) -> bool;
}

/// Outcome of one [`CdcBurstServe::serve_once`] iteration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BurstOutcome {
    /// Bytes the transport read returned this iteration (0 on idle).
    pub bytes_read: usize,
    /// Responses whose echo/notify/response reached the wire.
    pub responses: u32,
    /// NAK frames written (`ServeConfig::nak_on_error` profiles).
    pub naks: u32,
    /// Responses dropped for exceeding the host read window (#153
    /// answer-or-silence) — nothing was written for them.
    pub late_dropped: u32,
    /// Response flushes where a frame write failed.
    pub write_failures: u32,
    /// The read returned no data: the idle path ran (poll + floor
    /// raise).
    pub idle: bool,
    /// An escape 0xD1 ack reached the wire — the caller must panic
    /// NOW (dump-and-retrieve). The loop stopped consuming the burst
    /// mid-way, exactly where the inline loop's `panic!` fired.
    pub snapshot_requested: bool,
}

/// The CDC-class burst serve driver: [`CcidSerialServer`] plus the
/// #153/#168 arrival-floor state the loop maintains across reads.
/// Construct once, then call [`Self::serve_once`] from the transport
/// event loop forever.
pub struct CdcBurstServe<D: NfcDriver + FrontendDiagnostics> {
    server: CcidSerialServer<D>,
    /// Conservative arrival lower bound for bytes that may have queued
    /// behind a blocking handler (#153, Codex rounds 2–3 on #158/#168).
    arrival_floor: u32,
    /// OTA-validity source tag for the serve-evidence log line
    /// (e.g. `"gempc-cdc"` — see [`crate::ota_validity`]).
    serve_source: &'static str,
}

impl<D: NfcDriver + FrontendDiagnostics> CdcBurstServe<D> {
    /// `now_ticks` seeds the arrival floor (capture it BEFORE the boot
    /// drain, like the on-device loop does).
    pub fn new(server: CcidSerialServer<D>, now_ticks: u32, serve_source: &'static str) -> Self {
        Self {
            server,
            arrival_floor: now_ticks,
            serve_source,
        }
    }

    pub fn server_mut(&mut self) -> &mut CcidSerialServer<D> {
        &mut self.server
    }

    pub fn into_server(self) -> CcidSerialServer<D> {
        self.server
    }

    /// One serve-loop iteration: read one burst (or run the idle path
    /// on a non-positive read), feed every byte through the server
    /// with the arrival-floor discipline, flush completed exchanges to
    /// the wire inline, and maintain the floor across iterations.
    ///
    /// `now` is the monotonic (wrapping) tick source
    /// (`xTaskGetTickCount()` on target); it is consulted at the same
    /// points the inline loop captured ticks: after the read returns
    /// (`read_end`), per fed byte, after each handler dispatch (the
    /// #153 expiry verdict), and on idle.
    ///
    /// `on_quiet` runs at the wire-quiet points where the inline loop
    /// drained the BLE ring (after each response flush and on idle) —
    /// never between a frame's echo and response.
    pub fn serve_once<T: BurstWire, F: Fn() -> u32>(
        &mut self,
        wire: &mut T,
        burst_buf: &mut [u8],
        read_timeout_ticks: u32,
        now: F,
        on_quiet: &mut impl FnMut(),
    ) -> BurstOutcome {
        let mut out = BurstOutcome::default();
        let read = wire.read_burst(burst_buf, read_timeout_ticks);
        let n = match read {
            Ok(n) if n > 0 => n,
            _ => {
                // Read idle — the empty read proves the ring drained:
                // later bytes are fresh (floor raise), and the due
                // presence poll runs here (never above a pending
                // response).
                out.idle = true;
                let now_idle = now();
                self.arrival_floor = now_idle;
                self.server.poll_if_due(now_idle);
                on_quiet();
                return out;
            }
        };
        out.bytes_read = n;

        // One read = one burst: every byte in it shares the floor
        // captured before it.
        let read_end = now();
        let floor = self.arrival_floor;
        for &byte in &burst_buf[..n] {
            let feed_tick = now();
            match self.server.feed_byte(byte, feed_tick) {
                ServeAction::Respond => {
                    // #153: the frame may have queued behind the
                    // previous handler — anchor it at the burst's
                    // arrival floor before judging expiry, then drop
                    // responses that outlived the host's read window
                    // instead of poisoning the stream.
                    self.server.refresh_host_deadline(floor);
                    let post = now();
                    if self.server.response_expired(post) {
                        self.server.record_late_response();
                        out.late_dropped += 1;
                        continue;
                    }
                    // GemPC Twin: echo the received frame, then the
                    // framed CCID response (notify between them when
                    // the profile produces one — sync-mode contract).
                    // #294 WS2: evidence fires ONLY when every frame
                    // went out — a failed write is not serve evidence.
                    let sent = wire.write_frame(self.server.echo())
                        && (self.server.notify().is_empty()
                            || wire.write_frame(self.server.notify()))
                        && wire.write_frame(self.server.response());
                    if sent {
                        out.responses += 1;
                        crate::ota_validity::note_serve_evidence(self.serve_source);
                    } else {
                        out.write_failures += 1;
                    }
                    on_quiet();
                    // Dump-and-retrieve: the 0xD1 ack is on the wire —
                    // stop consuming the burst; the caller panics now
                    // so the flash coredump captures live state.
                    if self.server.take_snapshot_request() {
                        out.snapshot_requested = true;
                        return out;
                    }
                }
                ServeAction::Nak => {
                    if wire.write_frame(self.server.nak_frame()) {
                        out.naks += 1;
                    } else {
                        out.write_failures += 1;
                    }
                }
                ServeAction::None => {}
            }
        }
        // Codex round 3 on #168: the floor may only advance on
        // evidence the queue drained. A SHORT read could not fill the
        // buffer — plausibly drained at read return, so later bytes
        // are fresh. A FULL read may leave older queued bytes behind
        // (retries piling during a handler past one burst): the floor
        // must stay put or their arrivals would be overstated.
        if n < burst_buf.len() {
            self.arrival_floor = read_end;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccid_handler::CcidHandler;
    use crate::ccid_serial_server::{ResponseWindows, ServeConfig};
    use crate::nfc::MockNfcDriver;
    use ccid_protocol::types::{PC_TO_RDR_ESCAPE, PC_TO_RDR_GET_SLOT_STATUS};
    use ccid_transport_serial::{calculate_lrc, CTRL_ACK, SYNC};
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// 1 kHz tick model (ticks == ms), like the serial-server tests:
    /// the #153 tight window is 2500 ticks here.
    const POLL_INTERVAL: u32 = 500;
    const BURST_LEN: usize = 64;

    fn mock_driver(card_present: bool) -> MockNfcDriver {
        let atr = [0x3B, 0x80, 0x01, 0x01];
        let apdu = [0x90, 0x00];
        MockNfcDriver::new(card_present, &atr, &apdu)
    }

    fn burst_serve(card_present: bool) -> CdcBurstServe<MockNfcDriver> {
        let mut driver = mock_driver(card_present);
        driver.init().unwrap();
        CdcBurstServe::new(
            crate::ccid_serial_server::CcidSerialServer::new(
                CcidHandler::new(driver),
                POLL_INTERVAL,
                ResponseWindows::from_tick_hz(1_000),
                0,
            ),
            0,
            "cdc-burst-test",
        )
    }

    /// NAK-on-error variant (UART-main style profile) through the same
    /// burst loop.
    fn burst_serve_nak(card_present: bool) -> CdcBurstServe<MockNfcDriver> {
        let mut driver = mock_driver(card_present);
        driver.init().unwrap();
        CdcBurstServe::new(
            crate::ccid_serial_server::CcidSerialServer::with_config(
                CcidHandler::new(driver),
                POLL_INTERVAL,
                ResponseWindows::from_tick_hz(1_000),
                0,
                ServeConfig {
                    nak_on_error: true,
                    ..ServeConfig::default()
                },
            ),
            0,
            "cdc-burst-test",
        )
    }

    /// Command frame: SYNC + ACK + 10-byte CCID header (dwLength=0) + LRC.
    fn command_frame(message_type: u8, seq: u8) -> [u8; 13] {
        let ccid = [
            message_type,
            0,
            0,
            0,
            0,   // type + dwLength = 0 (LE)
            0,   // bSlot
            seq, // bSeq
            0,
            0,
            0, // bBWI + RFU
        ];
        let mut frame = [0u8; 13];
        frame[0] = SYNC;
        frame[1] = CTRL_ACK;
        frame[2..12].copy_from_slice(&ccid);
        frame[12] = calculate_lrc(&frame[..12]);
        frame
    }

    fn escape_frame(payload: &[u8], seq: u8) -> Vec<u8> {
        let mut ccid = [0u8; 10];
        ccid[0] = PC_TO_RDR_ESCAPE;
        ccid[1..5].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        ccid[6] = seq;
        let mut frame = Vec::with_capacity(2 + 10 + payload.len() + 1);
        frame.push(SYNC);
        frame.push(CTRL_ACK);
        frame.extend_from_slice(&ccid);
        frame.extend_from_slice(payload);
        frame.push(calculate_lrc(&frame));
        frame
    }

    /// Mock transport with a shared tick cell: each scripted burst
    /// carries the tick at which its read returns (the mock sets the
    /// clock during `read_burst`); the `now` closure handed to
    /// `serve_once` reads the same cell, so every tick capture inside
    /// the iteration is deterministic.
    struct MockWire {
        rx: VecDeque<(Vec<u8>, u32)>,
        writes: Vec<Vec<u8>>,
        fail_writes: bool,
        clock: Rc<Cell<u32>>,
    }

    impl MockWire {
        fn new(clock: Rc<Cell<u32>>) -> Self {
            Self {
                rx: VecDeque::new(),
                writes: Vec::new(),
                fail_writes: false,
                clock,
            }
        }

        /// One burst of bytes whose read returns at `read_end_tick`.
        fn burst(&mut self, bytes: &[u8], read_end_tick: u32) {
            self.rx.push_back((bytes.to_vec(), read_end_tick));
        }

        /// A read that returns empty at `idle_tick`.
        fn idle(&mut self, idle_tick: u32) {
            self.rx.push_back((Vec::new(), idle_tick));
        }
    }

    /// Tick source for `serve_once` — shares the cell with the mock so
    /// a scripted burst drives every tick capture inside the iteration.
    fn tick_source(clock: Rc<Cell<u32>>) -> impl Fn() -> u32 {
        move || clock.get()
    }

    impl BurstWire for MockWire {
        fn read_burst(
            &mut self,
            buf: &mut [u8],
            _timeout_ticks: u32,
        ) -> Result<usize, BurstReadError> {
            match self.rx.pop_front() {
                Some((bytes, tick)) => {
                    self.clock.set(tick);
                    let n = bytes.len().min(buf.len());
                    buf[..n].copy_from_slice(&bytes[..n]);
                    Ok(n)
                }
                None => Ok(0),
            }
        }

        fn write_frame(&mut self, bytes: &[u8]) -> bool {
            if self.fail_writes {
                return false;
            }
            self.writes.push(bytes.to_vec());
            true
        }
    }

    #[test]
    fn whole_frame_in_one_burst_answers_echo_then_response() {
        // The #119 core shape: a whole 13-byte frame in ONE read (the
        // burst that replaced 13 single-byte reads). One Respond,
        // echo first, framed response second — nothing else.
        let mut serve = burst_serve(false);
        let clock = Rc::new(Cell::new(0));
        let mut wire = MockWire::new(clock.clone());
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 7);
        wire.burst(&frame, 1_000);

        let out = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );

        assert_eq!(out.bytes_read, 13);
        assert_eq!(out.responses, 1);
        assert!(!out.idle && !out.snapshot_requested && out.late_dropped == 0);
        assert_eq!(wire.writes.len(), 2, "echo + response only");
        assert_eq!(wire.writes[0], &frame, "echo is the frame verbatim");
        let resp = &wire.writes[1];
        assert_eq!(resp[0], SYNC);
        assert_eq!(resp[1], CTRL_ACK);
        assert_eq!(resp[2], 0x81, "RDR_to_PC_SlotStatus");
        assert_eq!(resp[8], 7, "bSeq echoed");
        assert_eq!(
            resp[resp.len() - 1],
            calculate_lrc(&resp[..resp.len() - 1]),
            "LRC valid"
        );
    }

    #[test]
    fn frame_split_across_bursts_responds_only_at_completion() {
        // The batching must not lose or duplicate responses when the
        // host frame straddles two reads: nothing on the first partial
        // burst, one exchange when the LRC byte lands in the second
        // (bursts close together — inside INTER_BYTE_STALL_TICKS, like
        // USB delivery jitter, not a stalled frame).
        let mut serve = burst_serve(false);
        let clock = Rc::new(Cell::new(0));
        let mut wire = MockWire::new(clock.clone());
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 3);
        wire.burst(&frame[..6], 1_000);
        wire.burst(&frame[6..], 1_005);

        let out1 = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );
        assert_eq!(
            (out1.responses, out1.late_dropped),
            (0, 0),
            "partial frame: silent"
        );
        assert!(wire.writes.is_empty());
        let out2 = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );
        assert_eq!(out2.responses, 1, "completed by the second burst");
        assert_eq!(wire.writes.len(), 2);
        assert_eq!(wire.writes[0], &frame, "echo is the WHOLE frame");
    }

    #[test]
    fn two_frames_in_one_burst_keep_pair_ordering() {
        // Bench-verified ordering lock (66109dd): two frames in one
        // USB write flush mid-burst — echoA, respA, echoB, respB —
        // never echoA, echoB, respA, respB (the host parser would
        // desync: it eats exactly one echo per response).
        let mut serve = burst_serve(false);
        let clock = Rc::new(Cell::new(0));
        let mut wire = MockWire::new(clock.clone());
        let frame_a = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        let frame_b = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 2);
        let mut both = Vec::new();
        both.extend_from_slice(&frame_a);
        both.extend_from_slice(&frame_b);
        wire.burst(&both, 1_000);

        let out = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );

        assert_eq!(out.responses, 2);
        assert_eq!(wire.writes.len(), 4);
        assert_eq!(wire.writes[0], &frame_a, "echoA");
        assert_eq!(wire.writes[1][2], 0x81, "respA");
        assert_eq!(wire.writes[1][8], 1, "respA carries bSeq A");
        assert_eq!(wire.writes[2], &frame_b, "echoB");
        assert_eq!(wire.writes[3][2], 0x81, "respB");
        assert_eq!(wire.writes[3][8], 2, "respB carries bSeq B");
    }

    #[test]
    fn short_read_advances_the_arrival_floor() {
        // #168 short-read rule: a burst that did NOT fill the buffer
        // plausibly drained the ring, so the floor rises to its read
        // tick — a later frame is judged against ITS own arrival, not
        // against an arbitrarily old floor.
        let mut serve = burst_serve(false);
        let clock = Rc::new(Cell::new(0));
        let mut wire = MockWire::new(clock.clone());
        // Burst 1 (short, 13 < 64) served at 1_000 → floor = 1_000.
        wire.burst(&command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1), 1_000);
        // Burst 2's read returns at 3_000 — past the tight window
        // measured from the construction floor 0 (deadline 2_500), but
        // FRESH measured from the short read's floor 1_000 (deadline
        // 3_500): must be served, not dropped.
        wire.burst(&command_frame(PC_TO_RDR_GET_SLOT_STATUS, 2), 3_000);

        let out1 = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );
        let out2 = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );

        assert_eq!((out1.responses, out2.responses), (1, 1));
        assert_eq!(out2.late_dropped, 0, "short read raised the floor");
        assert_eq!(wire.writes.len(), 4);
    }

    #[test]
    fn full_read_keeps_the_arrival_floor() {
        // #168 full-read rule: a burst that FILLED the buffer may
        // leave older queued bytes in the ring — the floor must stay
        // put or their arrivals would be overstated. A frame arriving
        // in a later burst is still judged against the OLD floor: 64
        // bytes of (garbage + frame) at 1_000, then a frame at 4_000 —
        // the tight window from floor 0 closed at 2_500 → silence.
        let mut serve = burst_serve(false);
        let clock = Rc::new(Cell::new(0));
        let mut wire = MockWire::new(clock.clone());
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        let mut full = vec![0xAA; BURST_LEN - frame.len()];
        full.extend_from_slice(&frame);
        wire.burst(&full, 1_000); // full read: floor stays 0
        wire.burst(&command_frame(PC_TO_RDR_GET_SLOT_STATUS, 2), 4_000);

        let out1 = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );
        let out2 = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );

        assert_eq!(out1.responses, 1, "first frame served (floor 0 fresh)");
        assert_eq!(
            (out2.responses, out2.late_dropped),
            (0, 1),
            "full read kept the floor — the 4_000 frame is judged against 0 and silenced"
        );
        assert_eq!(wire.writes.len(), 2, "only the first exchange on the wire");
    }

    #[test]
    fn idle_read_raises_the_floor_and_polls_presence() {
        // Idle arm: the empty read is the drained-ring PROOF (floor
        // rises to now) and the due presence poll runs there.
        let mut serve = burst_serve(true);
        let clock = Rc::new(Cell::new(0));
        let mut wire = MockWire::new(clock.clone());
        wire.idle(POLL_INTERVAL + 10);
        let polls_before = serve.server_mut().handler_mut().driver_mut().poll_count();

        let out = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );

        assert!(out.idle);
        assert_eq!(
            serve.server_mut().handler_mut().driver_mut().poll_count(),
            polls_before + 1,
            "idle read ran the due presence poll"
        );
        // Floor raised: the next short-read frame is fresh against it
        // (read at 3_000 — deadline from the raised floor 510 is
        // 3_010, from the construction floor 0 it would be 2_500).
        wire.burst(&command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1), 3_000);
        let out2 = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );
        assert_eq!(
            (out2.responses, out2.late_dropped),
            (1, 0),
            "floor raised by the idle read — the later frame is judged fresh"
        );
    }

    #[test]
    fn queued_retry_behind_a_stale_floor_is_dropped_not_written() {
        // The #153/#158-R2 core scenario, end to end through the burst
        // loop: a host retry queued behind an over-window handler is
        // fed at drain time with FRESH-looking stamps; the floor
        // re-anchor makes the verdict honest and NOTHING reaches the
        // wire (late bytes poison the pcscd stream).
        let mut serve = burst_serve(false);
        let clock = Rc::new(Cell::new(0));
        let mut wire = MockWire::new(clock.clone());
        // Frame A served at 1_000; burst was short → floor = 1_000.
        wire.burst(&command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1), 1_000);
        // The retry sits queued across the whole window: read returns
        // it at 13_000 — its feed stamps look fresh (13_000), but the
        // floor (1_000) re-anchors the deadline to 3_500 → late.
        wire.burst(&command_frame(PC_TO_RDR_GET_SLOT_STATUS, 2), 13_000);

        let out1 = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );
        let out2 = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );

        assert_eq!(out1.responses, 1);
        assert_eq!((out2.responses, out2.late_dropped), (0, 1));
        assert_eq!(
            wire.writes.len(),
            2,
            "only frame A's echo+response — the retry produced zero wire bytes"
        );
        assert_eq!(serve.server_mut().late_drop_count(), 1);
    }

    #[test]
    fn write_failure_counts_and_is_not_serve_evidence() {
        // #294 WS2 through the burst loop: a failed frame write is not
        // serve evidence — an ARMED OTA gate stays untripped when the
        // wire write fails (and the response is still counted as
        // consumed, not retried).
        use crate::ota_validity::test_support::{reset_global_gate, GATE_TEST_LOCK};
        use core::sync::atomic::{AtomicUsize, Ordering};

        static MARKS: AtomicUsize = AtomicUsize::new(0);
        fn marker() -> i32 {
            MARKS.fetch_add(1, Ordering::SeqCst);
            0
        }

        let _lock = GATE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_global_gate();
        crate::ota_validity::install_mark_valid(marker);

        let mut serve = burst_serve(false);
        let clock = Rc::new(Cell::new(0));
        let mut wire = MockWire::new(clock.clone());
        wire.fail_writes = true;
        wire.burst(&command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1), 1_000);

        let out = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );

        assert_eq!((out.responses, out.write_failures), (0, 1));
        assert!(wire.writes.is_empty());
        assert_eq!(
            MARKS.load(Ordering::SeqCst),
            0,
            "failed write must not trip the OTA validity gate"
        );

        // Contrast: a successful flush DOES trip it (once).
        wire.fail_writes = false;
        wire.burst(&command_frame(PC_TO_RDR_GET_SLOT_STATUS, 2), 1_100);
        let out2 = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );
        assert_eq!(out2.responses, 1);
        assert_eq!(
            MARKS.load(Ordering::SeqCst),
            1,
            "delivered exchange trips the gate once"
        );

        reset_global_gate();
    }

    #[test]
    fn escape_d1_snapshot_stops_the_burst_midway() {
        // Dump-and-retrieve through the burst loop: the 0xD1 ack goes
        // out (echo + response), then the loop STOPS consuming the
        // burst — trailing bytes die with the caller's panic, exactly
        // where the inline loop panicked.
        let mut serve = burst_serve(false);
        let clock = Rc::new(Cell::new(0));
        let mut wire = MockWire::new(clock.clone());
        let mut burst = escape_frame(&[0xD1], 1);
        burst.extend_from_slice(&command_frame(PC_TO_RDR_GET_SLOT_STATUS, 2));
        wire.burst(&burst, 1_000);

        let out = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );

        assert!(out.snapshot_requested, "caller must panic for the coredump");
        assert_eq!(out.responses, 1, "the 0xD1 ack itself was flushed");
        assert_eq!(wire.writes.len(), 2, "echo + ack response only");
        assert_eq!(wire.writes[0], &burst[..burst.len() - 13]);
    }

    #[test]
    fn nak_profile_writes_nak_for_malformed_burst() {
        // Genericity guard: the burst loop also serves NAK-on-error
        // profiles (the wire-profile knob stays with the caller's
        // CcidSerialServer config).
        let mut serve = burst_serve_nak(false);
        let clock = Rc::new(Cell::new(0));
        let mut wire = MockWire::new(clock.clone());
        let mut bad = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        bad[12] ^= 0xFF;
        wire.burst(&bad, 1_000);

        let out = serve.serve_once(
            &mut wire,
            &mut [0u8; BURST_LEN],
            100,
            tick_source(clock.clone()),
            &mut || {},
        );

        assert_eq!(out.naks, 1);
        assert_eq!(wire.writes.len(), 1);
        assert_eq!(wire.writes[0], &[0x03, 0x15, 0x16], "the NAK frame");
    }
}
