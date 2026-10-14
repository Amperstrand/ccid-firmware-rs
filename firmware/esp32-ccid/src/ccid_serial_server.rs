//! Host-testable GemPC Twin serial CCID serving core.
//!
//! Extracted from the `pn7160-ccid` USB-CDC main loop so the byte-level
//! serving behavior (echo, framed response, time-gated card polling) is
//! unit-testable without hardware. Transport and clock are injected by the
//! caller; this module has no ESP-IDF dependencies.
//!
//! Wire behavior intentionally preserves the on-device loop as verified by
//! the on-target CCID tests (13/13 on ai-legion, `tests/test_ccid.py`):
//!
//! - complete commands are answered with the received frame echoed first
//!   (GemPC Twin protocol), then the framed CCID response — two writes
//! - parse errors (bad LRC/CTRL, oversized payload) are silently dropped
//!   and the parser resynchronizes on the next SYNC byte; unlike the UART
//!   main (`main.rs`), which NAKs malformed frames — do not "fix" this
//!   without re-validating against libccidtwin on hardware
//! - card presence is re-polled at most once per `poll_interval_ticks`,
//!   gated on GetSlotStatus commands and on read idle

use crate::ccid_handler::CcidHandler;
use crate::nfc::NfcDriver;
use ccid_protocol::types::{PC_TO_RDR_GET_SLOT_STATUS, PC_TO_RDR_XFR_BLOCK};
use ccid_transport_serial::{
    build_nak_frame, build_response_frame, build_slot_change_notification, FrameEvent, FrameParser,
};

/// Max CCID message (short APDU): 10-byte header + 261-byte payload.
pub const MAX_CCID_RESPONSE_SIZE: usize = 271;
/// SYNC + CTRL + CCID message + LRC.
const MAX_FRAME_SIZE: usize = 2 + MAX_CCID_RESPONSE_SIZE + 1;

/// (issue #156 B) CCID serial time-extension bridge.
///
/// The PN7160 stall hook runs deep inside the blocking card exchange
/// (`reader::exchange_detailed` → `StallHook`), where no references to
/// the serve loop or its transport exist. The mains register a raw
/// wire writer here at boot, the server arms the bridge with the
/// in-flight XfrBlock identity + echo frame at dispatch, and
/// [`stall_hook`] (installed into the PN7160 driver) writes the interim
/// [RDR_to_PC_DataBlock] time-extension frame.
///
/// Wire order is load-bearing (libccidtwin `ReadSerial`,
/// reference/CCID/src/ccid_serial.c): the ECHOED command frame is
/// consumed and discarded first (`if (echo) { echo = false; goto
/// start; }`) — interim frames must land AFTER the echo, so the hook
/// writes the stashed echo before the first extension frame and the
/// server then suppresses its own echo write. The host honors the
/// frame verbatim (commands.c `CCID_Receive`: `cmd[STATUS_OFFSET] &
/// CCID_TIME_EXTENSION` → `readTimeout *= cmd[ERROR_OFFSET]`, re-read
/// — the serial reader hands it ANY framed RDR message, the bStatus
/// bit, not the message type, keys the extension).
///
/// CCID Rev 1.1 §6.1.4 (time extension on PC_to_RDR_XfrBlock): the
/// interim frame KEEPS THE IN-FLIGHT COMMAND'S RESPONSE TYPE — an
/// empty RDR_to_PC_DataBlock (dwLength = 0) carrying the bSeq and
/// clock-status bytes; bStatus bit 7 set, bError = the number of BWT
/// units requested.
///
/// Single-tasked by construction (the hook fires on the serve task,
/// inside `feed_byte`) — the atomics exist for shape simplicity and
/// deterministic host tests, not cross-task safety.
pub mod time_extension {
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
    use std::sync::Mutex;

    use ccid_transport_serial::{calculate_lrc, CTRL_ACK, SYNC};

    use crate::ccid_types::RDR_TO_PC_DATABLOCK;

    /// bError multiplier: libccidtwin multiplies its remaining read
    /// timeout by this value per extension frame (commands.c:1598) —
    /// 3 s × 2 comfortably covers the 2.4 s exchange budget's tail.
    const BWT_UNIT_MULTIPLIER: u8 = 2;

    /// Wire size of one framed interim DataBlock: SYNC + CTRL + the
    /// 10-byte CCID header (dwLength = 0) + LRC.
    pub const FRAME_SIZE: usize = 13;

    static ARMED: AtomicBool = AtomicBool::new(false);
    static SEQ: AtomicU8 = AtomicU8::new(0);
    static ICC: AtomicU8 = AtomicU8::new(0);
    static ECHO_FLUSHED: AtomicBool = AtomicBool::new(false);
    static WRITER: Mutex<Option<fn(&[u8])>> = Mutex::new(None);
    static ECHO: Mutex<(usize, [u8; 2 + 261 + 3])> = Mutex::new((0, [0; 2 + 261 + 3]));

    /// Mains call this once at boot with their transport's raw write.
    pub fn register_writer(w: fn(&[u8])) {
        *WRITER.lock().unwrap() = Some(w);
    }

    pub fn writer_registered() -> bool {
        WRITER.lock().unwrap().is_some()
    }

    /// Server-side: stash the in-flight XfrBlock identity and echo.
    pub fn arm(seq: u8, icc: u8, echo: &[u8]) {
        SEQ.store(seq, Ordering::SeqCst);
        ICC.store(icc, Ordering::SeqCst);
        ECHO_FLUSHED.store(false, Ordering::SeqCst);
        let mut slot = ECHO.lock().unwrap();
        let n = echo.len().min(slot.1.len());
        slot.1[..n].copy_from_slice(&echo[..n]);
        slot.0 = n;
        ARMED.store(true, Ordering::SeqCst);
    }

    /// Server-side after dispatch: true when the hook already wrote the
    /// echo (suppress the serve loop's own echo write).
    pub fn disarm_and_take_flushed() -> bool {
        ARMED.store(false, Ordering::SeqCst);
        ECHO_FLUSHED.swap(false, Ordering::SeqCst)
    }

    /// The [`pn7160_nci::reader::StallHook`] target: emit the interim
    /// time-extension frame (echo first if not yet on the wire).
    pub fn stall_hook(_elapsed_ms: u32) {
        if !ARMED.load(Ordering::SeqCst) {
            return;
        }
        let writer = match *WRITER.lock().unwrap() {
            Some(w) => w,
            None => return,
        };
        if !ECHO_FLUSHED.swap(true, Ordering::SeqCst) {
            let slot = ECHO.lock().unwrap();
            writer(&slot.1[..slot.0]);
        }
        let mut frame = [0u8; FRAME_SIZE];
        let n = build_frame(
            SEQ.load(Ordering::SeqCst),
            ICC.load(Ordering::SeqCst),
            &mut frame,
        );
        writer(&frame[..n]);
    }

    /// Build one framed interim time-extension message (CCID Rev 1.1
    /// §6.1.4): an XfrBlock time extension KEEPS THE IN-FLIGHT
    /// COMMAND'S RESPONSE TYPE — RDR_to_PC_DataBlock (0x80), not
    /// SlotStatus. Empty DataBlock: dwLength = 0, bSeq of the
    /// in-flight XfrBlock, bStatus = 0x80 | icc status (time extension
    /// requested), bError = BWT unit multiplier, bClockStatus = 0,
    /// GemPC framing + LRC.
    pub fn build_frame(seq: u8, icc: u8, out: &mut [u8; FRAME_SIZE]) -> usize {
        out[0] = SYNC;
        out[1] = CTRL_ACK;
        out[2] = RDR_TO_PC_DATABLOCK;
        out[3] = 0;
        out[4] = 0;
        out[5] = 0;
        out[6] = 0; // dwLength = 0 (LE)
        out[7] = 0; // bSlot
        out[8] = seq; // bSeq — the in-flight XfrBlock's
        out[9] = 0x80 | (icc & 0x07); // bStatus: time extension | ICC status
        out[10] = BWT_UNIT_MULTIPLIER; // bError: BWT units requested
        out[11] = 0;
        out[12] = 0; // bClockStatus + RFU
        out[FRAME_SIZE - 1] = calculate_lrc(&out[..FRAME_SIZE - 1]);
        FRAME_SIZE
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeAction {
    /// A complete command was served: write [`Self::echo()`] first, then
    /// [`Self::notify()`] if present, then [`Self::response()`].
    Respond,
    /// A malformed frame arrived and [`ServeConfig::nak_on_error`] is on:
    /// write [`Self::nak_frame()`].
    Nak,
    /// No output: partial frame, or a dropped parse error (silent mode).
    None,
}

/// Wire-behavior profile — the differences between the UART mains and the
/// USB-CDC main that the serving core now owns instead of three copies in
/// `main.rs` (issue #90):
///
/// - UART mains NAK malformed frames and emit NotifySlotChange on poll
///   transitions (libccidtwin's ReadSerial path expects both; the
///   behaviors were hardware-validated across months of bench runs)
/// - USB-CDC main drops parse errors silently and never notifies (its
///   13/13 on-target tests passed exactly with these semantics)
/// - `inline_poll` (default `true`): GetSlotStatus may trigger the
///   physical presence poll inline. The m5stick disables it (#89):
///   MFRC522 polls on marginal coupling block for hundreds of ms
///   (WUPA timeouts + escalation), wedging the response. With
///   `inline_poll: false`, GetSlotStatus answers from cached presence,
///   physical polls run only on read-idle (no response pending), and
///   transitions detected there are delivered as NotifySlotChange in
///   the NEXT command window (echo → notify → response) — preserving
///   the sync-mode contract "after the host command and _before_ the
///   reader answer".
#[derive(Debug, Clone, Copy)]
pub struct ServeConfig {
    pub nak_on_error: bool,
    pub notify_slot_change: bool,
    pub inline_poll: bool,
}

impl Default for ServeConfig {
    fn default() -> Self {
        // The USB-CDC (nucula) profile: silent on parse errors, never
        // notifies, and keeps inline GetSlotStatus polls — its presence
        // behavior (TTL re-arm, #88) is bench-verified with this timing.
        Self {
            nak_on_error: false,
            notify_slot_change: false,
            inline_poll: true,
        }
    }
}

pub struct CcidSerialServer<D: NfcDriver> {
    handler: CcidHandler<D>,
    parser: FrameParser,
    config: ServeConfig,
    pending_notify: Option<bool>,
    echo_buf: [u8; MAX_FRAME_SIZE],
    notify_buf: [u8; 2],
    notify_len: usize,
    nak_buf: [u8; 3],
    last_byte_tick: u32,
    echo_len: usize,
    ccid_resp_buf: [u8; MAX_CCID_RESPONSE_SIZE],
    resp_buf: [u8; MAX_FRAME_SIZE],
    resp_len: usize,
    poll_interval_ticks: u32,
    last_poll_tick: u32,
}

impl<D: NfcDriver> CcidSerialServer<D> {
    /// Escape 0xD1 passthrough: true once, after the ack response was
    /// consumed — the caller panics then, so the flash coredump is written
    /// with the ack already on the wire (AGENTS.md "Crash dumps & snapshots").
    pub fn take_snapshot_request(&mut self) -> bool {
        self.handler.take_snapshot_request()
    }

    /// `now_ticks` seeds the poll gate the same way the on-device loop
    /// captures the boot tick, so the first poll fires only after one full
    /// interval.
    pub fn new(handler: CcidHandler<D>, poll_interval_ticks: u32, now_ticks: u32) -> Self {
        Self::with_config(
            handler,
            poll_interval_ticks,
            now_ticks,
            ServeConfig::default(),
        )
    }

    pub fn with_config(
        handler: CcidHandler<D>,
        poll_interval_ticks: u32,
        now_ticks: u32,
        config: ServeConfig,
    ) -> Self {
        Self {
            handler,
            config,
            parser: FrameParser::new(),
            pending_notify: None,
            notify_buf: [0; 2],
            notify_len: 0,
            nak_buf: [0; 3],
            echo_buf: [0; MAX_FRAME_SIZE],
            last_byte_tick: now_ticks,
            echo_len: 0,
            ccid_resp_buf: [0; MAX_CCID_RESPONSE_SIZE],
            resp_buf: [0; MAX_FRAME_SIZE],
            resp_len: 0,
            poll_interval_ticks: poll_interval_ticks.max(1),
            last_poll_tick: now_ticks,
        }
    }

    /// Feed one byte received from the host. `now_ticks` is a monotonic
    /// (wrapping) tick source such as FreeRTOS `xTaskGetTickCount()`.
    /// Bytes of a valid frame arrive back-to-back (USB batching). A gap
    /// this large (ticks == ms at the 1000 Hz tick rate) means the frame
    /// began with garbage: a truncated header otherwise eats the NEXT
    /// valid frame while the parser waits for bytes that never come
    /// (fuzz-proven, 2026-10-09).
    const INTER_BYTE_STALL_TICKS: u32 = 10;

    pub fn feed_byte(&mut self, byte: u8, now_ticks: u32) -> ServeAction {
        let gap = now_ticks.wrapping_sub(self.last_byte_tick);
        self.last_byte_tick = now_ticks;
        if gap > Self::INTER_BYTE_STALL_TICKS {
            self.parser.reset();
        }
        let ccid_bytes = match self.parser.feed(byte) {
            Some(FrameEvent::Command { ccid_bytes }) => ccid_bytes,
            Some(FrameEvent::Error(_)) => {
                if self.config.nak_on_error {
                    self.handler.record_nak();
                    build_nak_frame(&mut self.nak_buf);
                    return ServeAction::Nak;
                }
                // Silent mode: the parser has reset itself and resyncs on
                // the next SYNC byte.
                return ServeAction::None;
            }
            None => return ServeAction::None,
        };

        // Echo the received frame (GemPC Twin protocol). The parser's
        // received-frame snapshot survives its post-event reset.
        let frame = self.parser.received_frame_bytes();
        self.echo_len = frame.len();
        self.echo_buf[..self.echo_len].copy_from_slice(frame);

        // Presence for GetSlotStatus: either a fresh inline poll (profiles
        // with `inline_poll`, e.g. the USB-CDC main) or the transition the
        // idle path already observed (`pending_notify`, m5stick #89). The
        // notification rides between echo and response per sync mode.
        self.notify_len = 0;
        if ccid_bytes.first() == Some(&PC_TO_RDR_GET_SLOT_STATUS) {
            let stale = self.pending_notify.take();
            let fresh = if self.config.inline_poll && self.poll_due(now_ticks) {
                self.handler.refresh_diagnostics(now_ticks);
                self.handler.check_card_change()
            } else {
                None
            };
            if let Some(present) = fresh.or(stale) {
                if self.config.notify_slot_change {
                    self.notify_len = build_slot_change_notification(present, &mut self.notify_buf);
                }
            }
        }

        // Fresh uptime for any 0xD0 in flight (Codex review on #81):
        // poll-gated refresh alone goes stale under continuous traffic.
        self.handler.refresh_diagnostics(now_ticks);

        // Time-extension bridge (issue #156 B): XfrBlocks on a stalling
        // NFC controller (PN7160 WTX churn) emit interim frames from
        // inside the blocking exchange via the registered stall hook.
        // The hook writes the echo itself before the first interim
        // frame (libccidtwin ReadSerial eats one frame as the echo),
        // so a post-dispatch flush flag suppresses the double write.
        let time_ext_armed =
            time_extension::writer_registered() && ccid_bytes.first() == Some(&PC_TO_RDR_XFR_BLOCK);
        if time_ext_armed {
            time_extension::arm(
                ccid_bytes.get(6).copied().unwrap_or(0),
                self.handler.slot_state().icc_status(),
                &self.echo_buf[..self.echo_len],
            );
        }

        let resp_len = self
            .handler
            .process_command(&ccid_bytes, &mut self.ccid_resp_buf);
        if time_ext_armed && time_extension::disarm_and_take_flushed() {
            self.echo_len = 0;
        }
        self.resp_len = build_response_frame(&self.ccid_resp_buf[..resp_len], &mut self.resp_buf);
        ServeAction::Respond
    }

    /// Read-idle path: poll card presence if the interval elapsed.
    /// Returns `true` when a poll ran.
    pub fn poll_if_due(&mut self, now_ticks: u32) -> bool {
        if self.poll_due(now_ticks) {
            self.handler.check_card_change();
            true
        } else {
            false
        }
    }

    fn poll_due(&mut self, now_ticks: u32) -> bool {
        if now_ticks.wrapping_sub(self.last_poll_tick) >= self.poll_interval_ticks {
            self.last_poll_tick = now_ticks;
            true
        } else {
            false
        }
    }

    /// Wire bytes to write first after [`ServeAction::Respond`]: the
    /// received frame, echoed verbatim.
    pub fn echo(&self) -> &[u8] {
        &self.echo_buf[..self.echo_len]
    }

    /// Wire bytes to write last after [`ServeAction::Respond`]: the
    /// framed CCID response (SYNC + ACK + CCID message + LRC).
    pub fn response(&self) -> &[u8] {
        &self.resp_buf[..self.resp_len]
    }

    /// Wire bytes to write between echo and response when a presence
    /// transition was observed (empty when none).
    pub fn notify(&self) -> &[u8] {
        &self.notify_buf[..self.notify_len]
    }

    /// Wire bytes for [`ServeAction::Nak`]: the 3-byte NAK frame.
    pub fn nak_frame(&self) -> &[u8] {
        &self.nak_buf
    }

    /// Read-idle path (UART mains): reset any partial frame (the 100ms
    /// read timeout means garbage mid-frame is abandoned), then poll card
    /// presence if the interval elapsed.
    pub fn on_read_idle(&mut self, now_ticks: u32) -> bool {
        self.parser.reset();
        self.poll_and_stash(now_ticks)
    }

    /// Post-flush poll for the `inline_poll = false` profile (Codex PR 98):
    /// presence used to refresh ONLY on read idle, so a host that keeps
    /// the wire busy (commands closer together than the read timeout —
    /// pcscd's status loop does exactly that) starved the poll
    /// indefinitely and GetSlotStatus answered cached presence forever.
    /// Called after the response flush: no response is pending anymore,
    /// so the #89 rule (a physical poll must never delay a pending
    /// response) still holds. Transitions ride the next command window
    /// via `pending_notify`, exactly like the idle path.
    pub fn poll_if_due_after_response(&mut self, now_ticks: u32) -> bool {
        self.poll_and_stash(now_ticks)
    }

    /// Run a due presence poll and stash any transition for the next
    /// command window. Returns `true` when a poll ran.
    fn poll_and_stash(&mut self, now_ticks: u32) -> bool {
        if self.poll_due(now_ticks) {
            self.handler.refresh_diagnostics(now_ticks);
            if let Some(present) = self.handler.check_card_change() {
                self.pending_notify = Some(present);
            }
            true
        } else {
            false
        }
    }

    pub fn handler_mut(&mut self) -> &mut CcidHandler<D> {
        &mut self.handler
    }

    pub fn into_handler(self) -> CcidHandler<D> {
        self.handler
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nfc::MockNfcDriver;
    use ccid_protocol::types::{
        PC_TO_RDR_ESCAPE, PC_TO_RDR_GET_SLOT_STATUS, PC_TO_RDR_ICC_POWER_ON,
    };
    use ccid_transport_serial::{calculate_lrc, SYNC};

    const POLL_INTERVAL: u32 = 500;

    fn mock_driver(card_present: bool) -> MockNfcDriver {
        let atr = [0x3B, 0x80, 0x01, 0x01];
        let apdu = [0x90, 0x00];
        MockNfcDriver::new(card_present, &atr, &apdu)
    }

    fn server_with(card_present: bool) -> CcidSerialServer<MockNfcDriver> {
        let mut driver = mock_driver(card_present);
        driver.init().unwrap();
        CcidSerialServer::new(CcidHandler::new(driver), POLL_INTERVAL, 0)
    }

    /// Build a host→reader command frame: SYNC + ACK + 10-byte CCID header
    /// (dwLength = 0) + LRC.
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
        frame[1] = ccid_transport_serial::CTRL_ACK;
        frame[2..12].copy_from_slice(&ccid);
        frame[12] = calculate_lrc(&frame[..12]);
        frame
    }

    fn feed_frame<D: NfcDriver>(
        server: &mut CcidSerialServer<D>,
        frame: &[u8],
        now_ticks: u32,
    ) -> ServeAction {
        let mut action = ServeAction::None;
        for (i, &b) in frame.iter().enumerate() {
            action = server.feed_byte(b, now_ticks + i as u32);
        }
        action
    }

    #[test]
    fn get_slot_status_round_trip_echo_then_response() {
        let mut server = server_with(false);
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 7);

        let action = feed_frame(&mut server, &frame, 0);

        assert_eq!(action, ServeAction::Respond);
        // Echo is the received frame verbatim.
        assert_eq!(server.echo(), &frame);
        // Response is a valid frame: SYNC + ACK + RDR_to_PC_SlotStatus (0x81)
        // with matching sequence number and a correct LRC.
        let resp = server.response();
        assert_eq!(resp[0], SYNC);
        assert_eq!(resp[1], ccid_transport_serial::CTRL_ACK);
        assert_eq!(resp[2], 0x81, "RDR_to_PC_SlotStatus");
        assert_eq!(resp[8], 7, "bSeq echoed");
        let n = resp.len();
        assert_eq!(resp[n - 1], calculate_lrc(&resp[..n - 1]));
    }

    #[test]
    fn partial_frame_produces_no_output() {
        let mut server = server_with(false);
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);

        for &b in &frame[..frame.len() - 1] {
            assert_eq!(server.feed_byte(b, 0), ServeAction::None);
        }
    }

    #[test]
    fn invalid_lrc_dropped_then_recovers() {
        let mut server = server_with(false);
        let mut bad = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        bad[12] ^= 0xFF;

        // Corrupt frame: silently dropped, no output at all.
        assert_eq!(feed_frame(&mut server, &bad, 0), ServeAction::None);

        // The very next valid frame is served normally.
        let good = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 2);
        assert_eq!(feed_frame(&mut server, &good, 0), ServeAction::Respond);
        assert_eq!(server.echo(), &good);
    }

    #[test]
    fn garbage_between_frames_is_ignored() {
        let mut server = server_with(false);

        for &b in &[0xAA, 0x00, 0xFF, 0x50] {
            assert_eq!(server.feed_byte(b, 0), ServeAction::None);
        }

        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        assert_eq!(feed_frame(&mut server, &frame, 0), ServeAction::Respond);
        // Echo is the clean frame only — leading garbage is not echoed.
        assert_eq!(server.echo(), &frame);
    }

    #[test]
    fn slot_status_poll_is_interval_gated() {
        let mut server = server_with(true);
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        let driver_polls_before = server.handler_mut().driver_mut().poll_count();

        // Within the interval: command served, but no driver poll.
        feed_frame(&mut server, &frame, 100);
        assert_eq!(
            server.handler_mut().driver_mut().poll_count(),
            driver_polls_before
        );

        // After the interval: exactly one poll, not two for two commands.
        feed_frame(&mut server, &frame, 100 + POLL_INTERVAL);
        assert_eq!(
            server.handler_mut().driver_mut().poll_count(),
            driver_polls_before + 1
        );
        feed_frame(&mut server, &frame, 100 + POLL_INTERVAL + 10);
        assert_eq!(
            server.handler_mut().driver_mut().poll_count(),
            driver_polls_before + 1
        );
    }

    #[test]
    fn idle_poll_fires_once_per_interval() {
        let mut server = server_with(true);

        assert!(!server.poll_if_due(POLL_INTERVAL - 1));
        assert!(server.poll_if_due(POLL_INTERVAL));
        assert!(!server.poll_if_due(POLL_INTERVAL + 1));
        assert!(server.poll_if_due(2 * POLL_INTERVAL));
    }

    #[test]
    fn power_on_with_card_returns_framed_atr() {
        let mut server = server_with(true);

        // Prime card presence first (hosts always query slot status before
        // powering on): one poll past the interval caches present=true.
        let status = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        feed_frame(&mut server, &status, POLL_INTERVAL + 1);

        let frame = command_frame(PC_TO_RDR_ICC_POWER_ON, 2);
        assert_eq!(
            feed_frame(&mut server, &frame, POLL_INTERVAL + 20),
            ServeAction::Respond
        );

        let resp = server.response();
        assert_eq!(resp[2], 0x80, "RDR_to_PC_DataBlock");
        assert_eq!(resp[8], 2, "bSeq echoed");
        // ATR bytes appear in the payload after the 10-byte CCID header.
        let payload = &resp[2 + 10..resp.len() - 1];
        assert_eq!(&payload[..4], &[0x3B, 0x80, 0x01, 0x01]);
        let n = resp.len();
        assert_eq!(resp[n - 1], calculate_lrc(&resp[..n - 1]));
    }

    #[test]
    fn first_poll_waits_one_full_interval_from_construction() {
        let mut server = CcidSerialServer::new(
            CcidHandler::new(mock_driver(true)),
            POLL_INTERVAL,
            1_000, // constructed "late" in uptime
        );

        // A command shortly after construction does not poll.
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        feed_frame(&mut server, &frame, 1_050);
        assert_eq!(server.handler_mut().driver_mut().poll_count(), 0);

        // One interval later it does.
        feed_frame(&mut server, &frame, 1_500);
        assert_eq!(server.handler_mut().driver_mut().poll_count(), 1);
    }

    #[test]
    fn nak_mode_naks_malformed_frames_and_counts() {
        // UART-main profile: bad LRC → NAK frame + record_nak
        let mut driver = mock_driver(false);
        driver.init().unwrap();
        let mut server = CcidSerialServer::with_config(
            CcidHandler::new(driver),
            POLL_INTERVAL,
            0,
            ServeConfig {
                nak_on_error: true,
                notify_slot_change: false,
                inline_poll: true,
            },
        );
        let mut bad = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        bad[12] ^= 0xFF;

        assert_eq!(feed_frame(&mut server, &bad, 0), ServeAction::Nak);
        assert_eq!(server.nak_frame(), &[0x03, 0x15, 0x16]);
        assert_eq!(
            server.handler_mut().diagnostics().nak_count,
            1,
            "NAK recorded in diagnostics"
        );

        // Recovery: the next valid frame serves normally.
        let good = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 2);
        assert_eq!(feed_frame(&mut server, &good, 0), ServeAction::Respond);
    }

    #[test]
    fn silent_mode_still_drops_errors() {
        // USB-CDC profile (default): parse errors produce no output at all
        let mut server = server_with(false);
        let mut bad = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        bad[12] ^= 0xFF;
        assert_eq!(feed_frame(&mut server, &bad, 0), ServeAction::None);
        assert_eq!(
            server.handler_mut().diagnostics().nak_count,
            0,
            "silent mode does not count NAKs"
        );
    }

    #[test]
    fn notify_mode_emits_slot_change_on_presence_transition() {
        // Legacy inline-poll profile: GetSlotStatus-triggered poll that flips
        // presence emits NotifySlotChange between echo and response
        let mut server = CcidSerialServer::with_config(
            CcidHandler::new(mock_driver(true)),
            POLL_INTERVAL,
            0,
            ServeConfig {
                nak_on_error: false,
                notify_slot_change: true,
                inline_poll: true,
            },
        );
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);

        // First poll past the interval: absent → present transition
        assert_eq!(
            feed_frame(&mut server, &frame, POLL_INTERVAL + 1),
            ServeAction::Respond
        );
        let notify = server.notify();
        assert_eq!(notify.len(), 2, "NotifySlotChange is a 2-byte frame");
        assert_eq!(notify[0], 0x50, "RDR_to_PC_NotifySlotChange");

        // No transition on the next poll: no notification
        assert_eq!(
            feed_frame(&mut server, &frame, 2 * POLL_INTERVAL + 1),
            ServeAction::Respond
        );
        assert_eq!(
            server.notify().len(),
            0,
            "no notification without a transition"
        );
    }

    /// m5stick UART-main profile (#89): no physical poll may ever block a
    /// pending response — GetSlotStatus answers from cached presence.
    fn m5stick_profile(card_present: bool) -> CcidSerialServer<MockNfcDriver> {
        let mut driver = mock_driver(card_present);
        driver.init().unwrap();
        CcidSerialServer::with_config(
            CcidHandler::new(driver),
            POLL_INTERVAL,
            0,
            ServeConfig {
                nak_on_error: true,
                notify_slot_change: true,
                inline_poll: false,
            },
        )
    }

    #[test]
    fn no_inline_poll_profile_never_polls_on_get_slot_status() {
        let mut server = m5stick_profile(true);
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        let polls_before = server.handler_mut().driver_mut().poll_count();

        // GetSlotStatus storms past the poll interval: still zero driver polls
        for seq in 2..6u8 {
            feed_frame(&mut server, &frame, POLL_INTERVAL * seq as u32 + 1);
        }
        assert_eq!(
            server.handler_mut().driver_mut().poll_count(),
            polls_before,
            "GetSlotStatus must not trigger a physical poll in this profile"
        );

        // ...but every command still gets answered
        let resp = server.response();
        assert_eq!(resp[2], 0x81, "RDR_to_PC_SlotStatus");
    }

    #[test]
    fn post_response_poll_keeps_presence_fresh_under_continuous_traffic() {
        // Codex PR 98: with inline_poll=false, read-idle was the ONLY
        // poll site — a host keeping the wire busy (commands closer than
        // the read timeout) starved presence indefinitely. The main loop
        // now polls after each response flush; the transition rides the
        // next command window (pending_notify), and feed_byte itself
        // still never polls (#89 intact, previous test).
        let mut server = m5stick_profile(true);
        server.handler_mut().driver_mut().set_card_present(false);
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        let polls_before = server.handler_mut().driver_mut().poll_count();

        // Continuous traffic: every command lands well inside the read
        // timeout, so on_read_idle would NEVER fire in this scenario.
        assert_eq!(
            feed_frame(&mut server, &frame, POLL_INTERVAL * 2 + 1),
            ServeAction::Respond
        );
        assert!(server.poll_if_due_after_response(POLL_INTERVAL * 2 + 2));

        // The card lands mid-storm; the next post-response poll sees it.
        server.handler_mut().driver_mut().set_card_present(true);
        assert_eq!(
            feed_frame(&mut server, &frame, POLL_INTERVAL * 3 + 1),
            ServeAction::Respond
        );
        assert!(server.poll_if_due_after_response(POLL_INTERVAL * 3 + 2));

        // Only the two explicit post-response polls ran — feed_byte
        // contributed none.
        assert_eq!(
            server.handler_mut().driver_mut().poll_count(),
            polls_before + 2
        );

        // The transition surfaces in the NEXT command window.
        assert_eq!(
            feed_frame(&mut server, &frame, POLL_INTERVAL * 4 + 1),
            ServeAction::Respond
        );
        assert_eq!(server.notify(), &[0x50, 0x03], "present notification");
    }

    #[test]
    fn idle_poll_transition_notified_in_next_command_window() {
        // Card appears while the line is idle: the idle poll detects it, the
        // NEXT command delivers NotifySlotChange between echo and response
        // (sync-mode contract), and the response carries the fresh presence.
        let mut server = m5stick_profile(true);
        server.handler_mut().driver_mut().set_card_present(false);
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);

        // Prime: one idle poll with no card (no transition recorded)
        assert!(server.on_read_idle(POLL_INTERVAL + 1));
        assert_eq!(server.notify().len(), 0);

        // Card lands on the coil; the idle poll sees the transition
        server.handler_mut().driver_mut().set_card_present(true);
        assert!(server.on_read_idle(2 * POLL_INTERVAL + 1));

        // Next command: notify between echo and response
        assert_eq!(
            feed_frame(&mut server, &frame, 2 * POLL_INTERVAL + 20),
            ServeAction::Respond
        );
        let notify = server.notify();
        assert_eq!(notify, &[0x50, 0x03], "present notification");

        // No transition since: the following command carries no notify
        assert_eq!(
            feed_frame(&mut server, &frame, 3 * POLL_INTERVAL + 20),
            ServeAction::Respond
        );
        assert_eq!(
            server.notify().len(),
            0,
            "pending notify consumed exactly once"
        );
    }

    #[test]
    fn idle_poll_transition_survives_a_non_status_command_in_between() {
        // The pending transition must wait for a command window, not be lost
        // to intermediate traffic that is not GetSlotStatus.
        let mut server = m5stick_profile(true);
        server.handler_mut().driver_mut().set_card_present(false);
        server.on_read_idle(POLL_INTERVAL + 1);

        server.handler_mut().driver_mut().set_card_present(true);
        assert!(server.on_read_idle(2 * POLL_INTERVAL + 1));

        // An Escape in between must neither emit nor consume the transition
        let escape = command_frame(PC_TO_RDR_ESCAPE, 5);
        feed_frame(&mut server, &escape, 2 * POLL_INTERVAL + 10);
        assert_eq!(server.notify().len(), 0);

        // The GetSlotStatus delivers it
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 6);
        feed_frame(&mut server, &frame, 2 * POLL_INTERVAL + 20);
        assert_eq!(server.notify(), &[0x50, 0x03]);
    }

    #[test]
    fn on_read_idle_resets_partial_frame_and_polls() {
        let mut server = CcidSerialServer::with_config(
            CcidHandler::new(mock_driver(true)),
            POLL_INTERVAL,
            0,
            ServeConfig::default(),
        );
        // Feed half a frame
        let frame = command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1);
        for &b in &frame[..6] {
            server.feed_byte(b, 0);
        }
        // Idle: parser resets (partial frame abandoned), poll fires
        assert!(server.on_read_idle(POLL_INTERVAL + 1));
        // The next full frame still parses (the reset didn't strand state)
        assert_eq!(
            feed_frame(&mut server, &frame, POLL_INTERVAL + 20),
            ServeAction::Respond
        );
    }

    #[test]
    fn uptime_refreshes_under_continuous_non_poll_traffic() {
        // Codex review on #81: poll-gated refresh alone left uptime stale
        // when commands stream without GetSlotStatus. Dispatch-time refresh
        // must keep time-varying diagnostics current regardless of traffic mix.
        let mut server = server_with(true);
        let frame = command_frame(PC_TO_RDR_ICC_POWER_ON, 1);

        feed_frame(&mut server, &frame, 1_000);
        let early = server.handler_mut().diagnostics().uptime_ticks;
        // feed_frame advances ticks per byte; uptime reflects the frame's
        // last dispatched byte, so it is at (and near) the base tick.
        assert!(early >= 1_000, "uptime recorded at dispatch ({early})");

        for seq in 2..6u8 {
            feed_frame(
                &mut server,
                &command_frame(PC_TO_RDR_ICC_POWER_ON, seq),
                5_000 + seq as u32,
            );
        }
        let late = server.handler_mut().diagnostics().uptime_ticks;
        assert!(
            late > early,
            "uptime advanced without any GetSlotStatus ({late} > {early})"
        );
        assert!(
            (5_005..5_005 + 32).contains(&late),
            "uptime tracks the last dispatched frame ({late})"
        );
    }

    mod time_extension_tests {
        use super::*;
        use crate::ccid_types::SlotState;
        use ccid_protocol::types::PC_TO_RDR_XFR_BLOCK;
        use std::sync::Mutex;

        static CAPTURED: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

        fn capture(bytes: &[u8]) {
            CAPTURED.lock().unwrap().push(bytes.to_vec());
        }

        /// Build a host→reader XfrBlock frame carrying one APDU byte.
        fn xfr_frame(seq: u8, payload: &[u8]) -> Vec<u8> {
            let mut ccid = vec![PC_TO_RDR_XFR_BLOCK];
            ccid.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            ccid.push(0);
            ccid.push(seq);
            ccid.push(0);
            ccid.push(0);
            ccid.push(0);
            ccid.extend_from_slice(payload);
            let mut frame = vec![SYNC, ccid_transport_serial::CTRL_ACK];
            frame.extend_from_slice(&ccid);
            let lrc = calculate_lrc(&frame);
            frame.push(lrc);
            frame
        }

        #[test]
        fn interim_frame_bytes_reference_locked() {
            // GREAT EXPECTATION (reference-locked bytes — do not "fix"
            // without re-verifying against the cited references):
            // CCID Rev 1.1 §6.1.4 time extension on an in-flight
            // PC_to_RDR_XfrBlock — the interim frame KEEPS THE IN-FLIGHT
            // COMMAND'S RESPONSE TYPE (RDR_to_PC_DataBlock, 0x80, with
            // dwLength = 0 carrying the bSeq + clock-status bytes);
            // bStatus bit 7 = time extension requested, bError = BWT
            // units. Honored verbatim by libccidtwin
            // (reference/CCID/src/commands.c CCID_Receive:
            // "Time extension requested" → readTimeout *= bError; the
            // serial reader hands over ANY framed RDR message — the
            // bStatus bit, not the message type, keys the extension,
            // verified in ccid_serial.c ReadSerial).
            let mut out = [0u8; time_extension::FRAME_SIZE];
            let n = time_extension::build_frame(0x2A, 0x00, &mut out);
            assert_eq!(n, 13);
            assert_eq!(
                &out[..12],
                &[
                    0x03, 0x06, // GemPC framing: SYNC + ACK
                    0x80, // RDR_to_PC_DataBlock — the XfrBlock's response type
                    0x00, 0x00, 0x00, 0x00, // dwLength = 0 (empty DataBlock)
                    0x00, // bSlot
                    0x2A, // bSeq — the in-flight XfrBlock's
                    0x80, // bStatus: time extension | ICC present+active
                    0x02, // bError: 2 BWT units requested
                    0x00, // bClockStatus
                ]
            );
            assert_eq!(out[12], calculate_lrc(&out[..12]));
        }

        #[test]
        fn stalled_xfrblock_puts_echo_then_extension_on_the_wire() {
            // Issue #156 B: a stalling XfrBlock exchange emits the echo
            // FIRST (libccidtwin ReadSerial consumes one frame as the
            // echo — ccid_serial.c `if (echo) { echo = false; ... }`),
            // then the interim time-extension frame carrying the
            // XfrBlock's bSeq; the serve loop's own echo write is
            // suppressed (no double echo) and the real response still
            // follows.
            CAPTURED.lock().unwrap().clear();
            time_extension::register_writer(capture);

            let mut driver = mock_driver(true);
            driver.init().unwrap();
            driver.set_stall_probe(Some(time_extension::stall_hook));
            let mut server = CcidSerialServer::new(CcidHandler::new(driver), POLL_INTERVAL, 0);
            // Prime presence + power on so the XfrBlock reaches
            // transmit_apdu (same priming as the power-on test above).
            feed_frame(
                &mut server,
                &command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1),
                POLL_INTERVAL + 1,
            );
            feed_frame(
                &mut server,
                &command_frame(PC_TO_RDR_ICC_POWER_ON, 2),
                POLL_INTERVAL + 20,
            );

            let frame = xfr_frame(7, &[0x00, 0xA4, 0x04, 0x00]);
            assert_eq!(feed_frame(&mut server, &frame, 10), ServeAction::Respond);

            let wire = CAPTURED.lock().unwrap();
            assert_eq!(wire.len(), 2, "echo + one interim frame");
            assert_eq!(wire[0], frame, "echo first, verbatim");
            let ext = &wire[1];
            assert_eq!(ext[2], 0x80, "interim mirrors the XfrBlock's response type");
            assert_eq!(ext[8], 7, "bSeq of the stalled XfrBlock");
            assert_eq!(ext[9], 0x80 | SlotState::PresentActive.icc_status());
            assert_eq!(ext[10], 0x02, "bError multiplier");
            assert_eq!(
                ext[13.min(ext.len()) - 1],
                calculate_lrc(&ext[..ext.len() - 1]),
                "LRC valid"
            );
            drop(wire);

            // The serve loop's echo was suppressed; the response stands.
            assert_eq!(server.echo().len(), 0, "echo already on the wire");
            let resp = server.response();
            assert_eq!(resp[2], 0x80, "RDR_to_PC_DataBlock still served");
        }

        #[test]
        fn unstalled_xfrblock_keeps_normal_wire_order() {
            // No stall probe: no interim frames, echo NOT suppressed —
            // byte-for-byte the pre-#156-B wire behavior.
            CAPTURED.lock().unwrap().clear();
            time_extension::register_writer(capture);

            let mut server = server_with(true);
            feed_frame(
                &mut server,
                &command_frame(PC_TO_RDR_GET_SLOT_STATUS, 1),
                POLL_INTERVAL + 1,
            );
            feed_frame(
                &mut server,
                &command_frame(PC_TO_RDR_ICC_POWER_ON, 2),
                POLL_INTERVAL + 20,
            );

            let frame = xfr_frame(9, &[0x00, 0xA4, 0x04, 0x00]);
            assert_eq!(feed_frame(&mut server, &frame, 10), ServeAction::Respond);

            assert!(CAPTURED.lock().unwrap().is_empty(), "nothing emitted");
            assert_eq!(server.echo(), &frame, "echo served by the loop");
        }
    }
}
