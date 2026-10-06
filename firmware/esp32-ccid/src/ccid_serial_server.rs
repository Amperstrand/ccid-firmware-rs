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
use ccid_transport_serial::{build_response_frame, FrameEvent, FrameParser};

/// Max CCID message (short APDU): 10-byte header + 261-byte payload.
pub const MAX_CCID_RESPONSE_SIZE: usize = 271;
/// SYNC + CTRL + CCID message + LRC.
const MAX_FRAME_SIZE: usize = 2 + MAX_CCID_RESPONSE_SIZE + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeAction {
    /// A complete command was served: write [`Self::echo()`] first, then
    /// [`Self::response()`] (two writes, matching the verified on-device
    /// order).
    Respond,
    /// No output: partial frame, or a dropped parse error.
    None,
}

pub struct CcidSerialServer<D: NfcDriver> {
    handler: CcidHandler<D>,
    parser: FrameParser,
    echo_buf: [u8; MAX_FRAME_SIZE],
    echo_len: usize,
    ccid_resp_buf: [u8; MAX_CCID_RESPONSE_SIZE],
    resp_buf: [u8; MAX_FRAME_SIZE],
    resp_len: usize,
    poll_interval_ticks: u32,
    last_poll_tick: u32,
}

impl<D: NfcDriver> CcidSerialServer<D> {
    /// `now_ticks` seeds the poll gate the same way the on-device loop
    /// captures the boot tick, so the first poll fires only after one full
    /// interval.
    pub fn new(handler: CcidHandler<D>, poll_interval_ticks: u32, now_ticks: u32) -> Self {
        Self {
            handler,
            parser: FrameParser::new(),
            echo_buf: [0; MAX_FRAME_SIZE],
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
    pub fn feed_byte(&mut self, byte: u8, now_ticks: u32) -> ServeAction {
        let ccid_bytes = match self.parser.feed(byte) {
            Some(FrameEvent::Command { ccid_bytes }) => ccid_bytes,
            // Parse errors: drop silently; the parser has reset itself and
            // resynchronizes on the next SYNC byte.
            Some(FrameEvent::Error(_)) | None => return ServeAction::None,
        };

        // Echo the received frame (GemPC Twin protocol). The parser's
        // received-frame snapshot survives its post-event reset.
        let frame = self.parser.received_frame_bytes();
        self.echo_len = frame.len();
        self.echo_buf[..self.echo_len].copy_from_slice(frame);

        // Time-gated card poll on GetSlotStatus (mirrors the on-device loop).
        if ccid_bytes.first() == Some(&PC_TO_RDR_GET_SLOT_STATUS) && self.poll_due(now_ticks) {
            self.handler.check_card_change();
        }

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

    /// Wire bytes to write second after [`ServeAction::Respond`]: the
    /// framed CCID response (SYNC + ACK + CCID message + LRC).
    pub fn response(&self) -> &[u8] {
        &self.resp_buf[..self.resp_len]
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
}
