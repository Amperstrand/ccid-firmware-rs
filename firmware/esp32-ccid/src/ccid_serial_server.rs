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
use ccid_protocol::types::PC_TO_RDR_GET_SLOT_STATUS;
use ccid_transport_serial::{
    build_nak_frame, build_response_frame, build_slot_change_notification, FrameEvent, FrameParser,
};

/// Max CCID message (short APDU): 10-byte header + 261-byte payload.
pub const MAX_CCID_RESPONSE_SIZE: usize = 271;
/// SYNC + CTRL + CCID message + LRC.
const MAX_FRAME_SIZE: usize = 2 + MAX_CCID_RESPONSE_SIZE + 1;

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
#[derive(Debug, Clone, Copy, Default)]
pub struct ServeConfig {
    pub nak_on_error: bool,
    pub notify_slot_change: bool,
}

pub struct CcidSerialServer<D: NfcDriver> {
    handler: CcidHandler<D>,
    parser: FrameParser,
    config: ServeConfig,
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

        // Time-gated card poll on GetSlotStatus (mirrors the UART mains):
        // a presence transition emits NotifySlotChange between echo and
        // response when the profile enables it.
        self.notify_len = 0;
        if ccid_bytes.first() == Some(&PC_TO_RDR_GET_SLOT_STATUS) && self.poll_due(now_ticks) {
            self.handler.refresh_diagnostics(now_ticks);
            if let Some(present) = self.handler.check_card_change() {
                if self.config.notify_slot_change {
                    self.notify_len = build_slot_change_notification(present, &mut self.notify_buf);
                }
            }
        }

        // Fresh uptime for any 0xD0 in flight (Codex review on #81):
        // poll-gated refresh alone goes stale under continuous traffic.
        self.handler.refresh_diagnostics(now_ticks);

        let resp_len = self
            .handler
            .process_command(&ccid_bytes, &mut self.ccid_resp_buf);
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
        if self.poll_due(now_ticks) {
            self.handler.refresh_diagnostics(now_ticks);
            self.handler.check_card_change();
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
    use ccid_protocol::types::{PC_TO_RDR_GET_SLOT_STATUS, PC_TO_RDR_ICC_POWER_ON};
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
        // UART-main profile: GetSlotStatus-triggered poll that flips
        // presence emits NotifySlotChange between echo and response
        let mut server = CcidSerialServer::with_config(
            CcidHandler::new(mock_driver(true)),
            POLL_INTERVAL,
            0,
            ServeConfig {
                nak_on_error: false,
                notify_slot_change: true,
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
}
