use heapless::Vec;

use crate::ccid_types::{
    CARD_ABSENT, CARD_PRESENT, CCID_HEADER_SIZE, MAX_CCID_MESSAGE_LENGTH,
    RDR_TO_PC_NOTIFY_SLOT_CHANGE,
};

pub const SYNC: u8 = 0x03;
pub const CTRL_ACK: u8 = 0x06;
pub const CTRL_NAK: u8 = 0x15;

const MAX_CCID_HEADER_LEN: usize = CCID_HEADER_SIZE;
const MAX_CCID_BYTES: usize = MAX_CCID_MESSAGE_LENGTH;
const MAX_CCID_PAYLOAD: usize = MAX_CCID_BYTES - MAX_CCID_HEADER_LEN;
const MAX_FRAME_BYTES: usize = 2 + MAX_CCID_BYTES + 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameEvent {
    Command { ccid_bytes: Vec<u8, MAX_CCID_BYTES> },
    Error(FrameError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    InvalidLrc,
    InvalidCtrl,
    Overflow,
    NakReceived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserState {
    WaitSync,
    WaitCtrl,
    ReadHeader(usize),
    ReadPayload(usize),
    CheckLrc,
}

#[derive(Debug, Clone)]
pub struct FrameParser {
    state: ParserState,
    frame: Vec<u8, MAX_FRAME_BYTES>,
    received_frame: Vec<u8, MAX_FRAME_BYTES>,
    ccid_bytes: Vec<u8, MAX_CCID_BYTES>,
    payload_len: usize,
}

pub fn calculate_lrc(frame: &[u8]) -> u8 {
    frame.iter().fold(0u8, |acc, byte| acc ^ byte)
}

pub fn build_response_frame(ccid_response: &[u8], buf: &mut [u8]) -> usize {
    let total_len = 2 + ccid_response.len() + 1;
    assert!(
        ccid_response.len() <= MAX_CCID_BYTES,
        "ccid response exceeds max size"
    );
    assert!(
        buf.len() >= total_len,
        "buffer too small for response frame"
    );

    buf[0] = SYNC;
    buf[1] = CTRL_ACK;
    buf[2..2 + ccid_response.len()].copy_from_slice(ccid_response);
    buf[total_len - 1] = calculate_lrc(&buf[..total_len - 1]);
    total_len
}

pub fn build_nak_frame(buf: &mut [u8]) -> usize {
    assert!(buf.len() >= 3, "buffer too small for nak frame");
    buf[0] = SYNC;
    buf[1] = CTRL_NAK;
    buf[2] = calculate_lrc(&buf[..2]);
    3
}

pub fn build_slot_change_notification(card_present: bool, buf: &mut [u8]) -> usize {
    assert!(
        buf.len() >= 2,
        "buffer too small for slot change notification"
    );
    buf[0] = RDR_TO_PC_NOTIFY_SLOT_CHANGE;
    buf[1] = if card_present {
        CARD_PRESENT
    } else {
        CARD_ABSENT
    };
    2
}

impl FrameParser {
    pub fn new() -> Self {
        Self {
            state: ParserState::WaitSync,
            frame: Vec::new(),
            received_frame: Vec::new(),
            ccid_bytes: Vec::new(),
            payload_len: 0,
        }
    }

    pub fn feed(&mut self, byte: u8) -> Option<FrameEvent> {
        match self.state {
            ParserState::WaitSync => {
                if byte == RDR_TO_PC_NOTIFY_SLOT_CHANGE {
                    self.reset();
                } else if byte == SYNC {
                    self.start_frame(byte);
                    self.state = ParserState::WaitCtrl;
                }
                None
            }
            ParserState::WaitCtrl => {
                self.push_frame_byte(byte);
                match byte {
                    CTRL_ACK => {
                        self.state = ParserState::ReadHeader(0);
                        None
                    }
                    CTRL_NAK => self.finish_with_error(FrameError::NakReceived),
                    _ => self.finish_with_error(FrameError::InvalidCtrl),
                }
            }
            ParserState::ReadHeader(read) => {
                self.push_frame_and_ccid_byte(byte);
                let next_read = read + 1;
                if next_read == MAX_CCID_HEADER_LEN {
                    self.payload_len = u32::from_le_bytes([
                        self.ccid_bytes[1],
                        self.ccid_bytes[2],
                        self.ccid_bytes[3],
                        self.ccid_bytes[4],
                    ]) as usize;

                    if self.payload_len > MAX_CCID_PAYLOAD {
                        return self.finish_with_error(FrameError::Overflow);
                    }

                    self.state = if self.payload_len == 0 {
                        ParserState::CheckLrc
                    } else {
                        ParserState::ReadPayload(self.payload_len)
                    };
                    None
                } else {
                    self.state = ParserState::ReadHeader(next_read);
                    None
                }
            }
            ParserState::ReadPayload(remaining) => {
                self.push_frame_and_ccid_byte(byte);
                if remaining == 1 {
                    self.state = ParserState::CheckLrc;
                } else {
                    self.state = ParserState::ReadPayload(remaining - 1);
                }
                None
            }
            ParserState::CheckLrc => {
                self.push_frame_byte(byte);
                let expected = calculate_lrc(&self.frame[..self.frame.len() - 1]);
                if byte == expected {
                    self.finish_with_command()
                } else {
                    self.finish_with_error(FrameError::InvalidLrc)
                }
            }
        }
    }

    pub fn received_frame_bytes(&self) -> &[u8] {
        if self.received_frame.is_empty() {
            &self.frame
        } else {
            &self.received_frame
        }
    }

    pub fn reset(&mut self) {
        self.received_frame.clear();
        self.reset_state();
    }

    fn reset_state(&mut self) {
        self.state = ParserState::WaitSync;
        self.frame.clear();
        self.ccid_bytes.clear();
        self.payload_len = 0;
    }

    fn start_frame(&mut self, sync: u8) {
        self.frame.clear();
        self.ccid_bytes.clear();
        self.payload_len = 0;
        self.push_frame_byte(sync);
        self.received_frame.clear();
    }

    fn push_frame_byte(&mut self, byte: u8) {
        let _ = self.frame.push(byte);
    }

    fn push_frame_and_ccid_byte(&mut self, byte: u8) {
        self.push_frame_byte(byte);
        let _ = self.ccid_bytes.push(byte);
    }

    fn snapshot_received_frame(&mut self) {
        self.received_frame.clear();
        let _ = self.received_frame.extend_from_slice(&self.frame);
    }

    fn finish_with_command(&mut self) -> Option<FrameEvent> {
        self.snapshot_received_frame();
        let ccid_bytes = self.ccid_bytes.clone();
        self.reset_state();
        Some(FrameEvent::Command { ccid_bytes })
    }

    fn finish_with_error(&mut self, error: FrameError) -> Option<FrameEvent> {
        self.snapshot_received_frame();
        self.reset_state();
        Some(FrameEvent::Error(error))
    }
}

impl Default for FrameParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec as StdVec;

    fn header_with_payload_len(message_type: u8, payload_len: u32) -> [u8; 10] {
        [
            message_type,
            payload_len as u8,
            (payload_len >> 8) as u8,
            (payload_len >> 16) as u8,
            (payload_len >> 24) as u8,
            0,
            0,
            0,
            0,
            0,
        ]
    }

    #[test]
    fn test_lrc_nak() {
        assert_eq!(calculate_lrc(&[0x03, 0x15]), 0x16);
    }

    #[test]
    fn test_calculate_lrc_full_frame_xor_is_zero() {
        let frame = [0x03, 0x06, 0x81, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let lrc = calculate_lrc(&frame);
        assert_eq!(calculate_lrc(&[&frame[..], &[lrc]].concat()), 0);
    }

    #[test]
    fn test_build_nak_frame() {
        let mut buf = [0u8; 8];
        let n = build_nak_frame(&mut buf);
        assert_eq!(n, 3);
        assert_eq!(&buf[..3], &[0x03, 0x15, 0x16]);
    }

    #[test]
    fn test_build_response_frame_empty_payload() {
        let ccid = [0x81u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut buf = [0u8; 274];
        let n = build_response_frame(&ccid, &mut buf);
        assert_eq!(n, 13);
        assert_eq!(buf[0], 0x03);
        assert_eq!(buf[1], 0x06);
        assert_eq!(&buf[2..12], &ccid);
        let expected_lrc = buf[..12].iter().fold(0u8, |acc, b| acc ^ b);
        assert_eq!(buf[12], expected_lrc);
    }

    #[test]
    fn test_build_response_frame_with_payload() {
        let ccid = [0x80u8, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0x90, 0x00];
        let mut buf = [0u8; 274];
        let n = build_response_frame(&ccid, &mut buf);
        assert_eq!(n, 15);
        assert_eq!(buf[14], calculate_lrc(&buf[..14]));
    }

    #[test]
    fn test_frame_parser_round_trip() {
        let ccid = [0x81u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut frame_buf = [0u8; 274];
        let n = build_response_frame(&ccid, &mut frame_buf);

        let mut parser = FrameParser::new();
        let mut event = None;
        for b in &frame_buf[..n] {
            event = parser.feed(*b);
            if event.is_some() {
                break;
            }
        }
        match event.unwrap() {
            FrameEvent::Command { ccid_bytes } => assert_eq!(&ccid_bytes[..], &ccid),
            FrameEvent::Error(e) => panic!("Expected command, got error: {:?}", e),
        }
        assert_eq!(parser.received_frame_bytes(), &frame_buf[..n]);
    }

    #[test]
    fn test_parser_invalid_lrc() {
        let ccid = [0x81u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut buf = [0u8; 274];
        let n = build_response_frame(&ccid, &mut buf);
        buf[n - 1] ^= 0xFF;

        let mut parser = FrameParser::new();
        let mut event = None;
        for b in &buf[..n] {
            event = parser.feed(*b);
            if event.is_some() {
                break;
            }
        }
        assert!(matches!(
            event,
            Some(FrameEvent::Error(FrameError::InvalidLrc))
        ));
        assert_eq!(parser.received_frame_bytes(), &buf[..n]);
    }

    #[test]
    fn test_slot_change_present() {
        let mut buf = [0u8; 8];
        let n = build_slot_change_notification(true, &mut buf);
        assert_eq!(n, 2);
        assert_eq!(buf[0], 0x50);
        assert_eq!(buf[1], 0x03);
    }

    #[test]
    fn test_slot_change_absent() {
        let mut buf = [0u8; 8];
        let n = build_slot_change_notification(false, &mut buf);
        assert_eq!(n, 2);
        assert_eq!(buf[0], 0x50);
        assert_eq!(buf[1], 0x02);
    }

    #[test]
    fn test_parser_with_payload() {
        let ccid: StdVec<u8> = {
            let mut v = StdVec::from(header_with_payload_len(0x80, 2));
            v.extend_from_slice(&[0x90, 0x00]);
            v
        };
        let mut buf = [0u8; 274];
        let n = build_response_frame(&ccid, &mut buf);

        let mut parser = FrameParser::new();
        let mut event = None;
        for b in &buf[..n] {
            event = parser.feed(*b);
            if event.is_some() {
                break;
            }
        }
        match event.unwrap() {
            FrameEvent::Command { ccid_bytes } => {
                assert_eq!(ccid_bytes.len(), 12);
                assert_eq!(ccid_bytes[10], 0x90);
                assert_eq!(ccid_bytes[11], 0x00);
            }
            FrameEvent::Error(e) => panic!("{:?}", e),
        }
    }

    #[test]
    fn test_parser_rejects_invalid_ctrl() {
        let mut parser = FrameParser::new();
        assert_eq!(parser.feed(SYNC), None);
        assert!(matches!(
            parser.feed(0x07),
            Some(FrameEvent::Error(FrameError::InvalidCtrl))
        ));
        assert_eq!(parser.received_frame_bytes(), &[SYNC, 0x07]);
    }

    #[test]
    fn test_parser_reports_nak_received() {
        let mut parser = FrameParser::new();
        assert_eq!(parser.feed(SYNC), None);
        assert!(matches!(
            parser.feed(CTRL_NAK),
            Some(FrameEvent::Error(FrameError::NakReceived))
        ));
        assert_eq!(parser.received_frame_bytes(), &[SYNC, CTRL_NAK]);
    }

    #[test]
    fn test_parser_ignores_slot_change_notification_prefix() {
        let mut parser = FrameParser::new();
        assert_eq!(parser.feed(RDR_TO_PC_NOTIFY_SLOT_CHANGE), None);
        assert!(parser.received_frame_bytes().is_empty());
        assert_eq!(parser.feed(SYNC), None);
    }

    #[test]
    fn test_parser_overflow_when_payload_too_large() {
        let oversized = header_with_payload_len(0x80, 262);
        let mut parser = FrameParser::new();

        assert_eq!(parser.feed(SYNC), None);
        assert_eq!(parser.feed(CTRL_ACK), None);
        let mut event = None;
        for byte in oversized {
            event = parser.feed(byte);
            if event.is_some() {
                break;
            }
        }

        assert!(matches!(
            event,
            Some(FrameEvent::Error(FrameError::Overflow))
        ));
        assert_eq!(
            parser.received_frame_bytes(),
            &[SYNC, CTRL_ACK, 0x80, 6, 1, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn test_parser_waits_for_full_frame_before_emitting() {
        let ccid = [0x81u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut frame_buf = [0u8; 274];
        let n = build_response_frame(&ccid, &mut frame_buf);
        let mut parser = FrameParser::new();

        for b in &frame_buf[..n - 1] {
            assert!(parser.feed(*b).is_none());
        }
        assert!(matches!(
            parser.feed(frame_buf[n - 1]),
            Some(FrameEvent::Command { .. })
        ));
    }

    #[test]
    fn test_parser_reset_clears_in_progress_state() {
        let mut parser = FrameParser::new();
        assert_eq!(parser.feed(SYNC), None);
        assert_eq!(parser.feed(CTRL_ACK), None);
        parser.reset();
        assert!(parser.received_frame_bytes().is_empty());
        assert_eq!(parser.feed(SYNC), None);
    }
}
