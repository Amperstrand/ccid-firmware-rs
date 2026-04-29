//! PPS (Protocol and Parameters Selection) State Machine
//!
//! ISO 7816-3 §9 compliant PPS negotiation with full state tracking.
//! Ported from osmo-ccid-firmware ccid_common/iso7816_fsm.c
//!
//! This is a synchronous state machine designed for embedded use:
//! - Call `PpsFsm::new()` to initialize
//! - Call `process_byte()` for each received byte
//! - Check `state()` for completion status

#![cfg(all(target_arch = "arm", target_os = "none"))]
#![allow(dead_code)]

/// PPS state machine states (matching osmo-ccid-firmware)
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum PpsState {
    /// Initial state - ready to build and send PPS request
    Init,
    /// Transmitting PPS request bytes
    TxRequest,
    /// Waiting for PPSS (initial byte 0xFF)
    WaitPpsx,
    /// Waiting for PPS0 (format byte)
    WaitPps0,
    /// Waiting for PPS1 (Fi/Di parameter)
    WaitPps1,
    /// Waiting for PPS2 (SPU - not supported, skip if present)
    WaitPps2,
    /// Waiting for PPS3 (reserved - not used)
    WaitPps3,
    /// Waiting for PCK (checksum)
    WaitPck,
    /// Negotiation complete successfully
    Done,
    /// Negotiation failed
    Failed,
}

/// PPS negotiation result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PpsResult {
    /// Success - response matched request
    Success,
    /// Failed - response didn't match request
    Mismatch,
    /// Unsupported - card didn't respond (timeout)
    Timeout,
    /// Error - parity or hardware error
    Error,
}

/// PPS request/response buffer (max 6 bytes: PPSS + PPS0-3 + PCK)
const PPS_MAX_LEN: usize = 6;

/// PPS State Machine
///
/// Implements ISO 7816-3 §9 PPS negotiation with the following flow:
/// 1. Build request: [PPSS=0xFF, PPS0, PPS1?, PPS2?, PPS3?, PCK]
/// 2. Transmit request
/// 3. Receive response byte-by-byte
/// 4. Validate response matches request
///
/// From osmo-ccid-firmware:
/// - PPS0 bit 4 = PPS1 present (Fi/Di from TA1)
/// - PPS0 bit 5 = PPS2 present (SPU - we don't support)
/// - PPS0 bit 6 = PPS3 present (reserved)
/// - PPS0 bits 0-3 = protocol (T=0 or T=1)
/// - PCK = XOR of all bytes including PPSS
pub struct PpsFsm {
    /// Current state
    state: PpsState,
    /// PPS request bytes (what we sent)
    tx_buf: [u8; PPS_MAX_LEN],
    /// PPS request length
    tx_len: usize,
    /// PPS response bytes (what we received)
    rx_buf: [u8; PPS_MAX_LEN],
    /// PPS response length
    rx_len: usize,
    /// PPS0 byte from response (determines which PPSx bytes follow)
    pps0_recv: u8,
    /// Negotiated Fi value
    pub fi: u16,
    /// Negotiated Di value
    pub di: u8,
    /// Negotiated protocol
    pub protocol: u8,
}

impl Default for PpsFsm {
    fn default() -> Self {
        Self::new()
    }
}

impl PpsFsm {
    /// Create a new PPS state machine in Init state
    pub fn new() -> Self {
        Self {
            state: PpsState::Init,
            tx_buf: [0u8; PPS_MAX_LEN],
            tx_len: 0,
            rx_buf: [0u8; PPS_MAX_LEN],
            rx_len: 0,
            pps0_recv: 0,
            fi: 372,
            di: 1,
            protocol: 0,
        }
    }

    /// Get current state
    pub fn state(&self) -> PpsState {
        self.state
    }

    /// Get the PPS request bytes (to transmit)
    pub fn request(&self) -> &[u8] {
        &self.tx_buf[..self.tx_len]
    }

    pub fn build_request(&mut self, protocol: u8, ta1: u8) -> &[u8] {
        self.tx_len = 0;
        self.rx_len = 0;
        self.pps0_recv = 0;
        self.protocol = protocol;

        self.tx_buf[self.tx_len] = 0xFF;
        self.tx_len += 1;

        let pps0 = 0x10u8 | (protocol & 0x0F);
        self.tx_buf[self.tx_len] = pps0;
        self.tx_len += 1;

        self.tx_buf[self.tx_len] = ta1;
        self.tx_len += 1;

        let mut pck = 0u8;
        for i in 0..self.tx_len {
            pck ^= self.tx_buf[i];
        }
        self.tx_buf[self.tx_len] = pck;
        self.tx_len += 1;

        self.state = PpsState::TxRequest;
        defmt::debug!(
            "PPS: built request len={} {:?}",
            self.tx_len,
            &self.tx_buf[..self.tx_len]
        );
        &self.tx_buf[..self.tx_len]
    }

    pub fn build_minimal_request(&mut self, protocol: u8) -> &[u8] {
        self.tx_len = 0;
        self.rx_len = 0;
        self.pps0_recv = 0;
        self.protocol = protocol;

        self.tx_buf[self.tx_len] = 0xFF;
        self.tx_len += 1;

        let pps0 = protocol & 0x0F;
        self.tx_buf[self.tx_len] = pps0;
        self.tx_len += 1;

        let mut pck = 0u8;
        for i in 0..self.tx_len {
            pck ^= self.tx_buf[i];
        }
        self.tx_buf[self.tx_len] = pck;
        self.tx_len += 1;

        self.state = PpsState::TxRequest;
        defmt::debug!(
            "PPS: built minimal request len={} {:?}",
            self.tx_len,
            &self.tx_buf[..self.tx_len]
        );
        &self.tx_buf[..self.tx_len]
    }

    /// Transition to waiting for response
    ///
    /// Call this after transmitting the PPS request
    pub fn start_response(&mut self) {
        self.state = PpsState::WaitPpsx;
        self.rx_len = 0;
        defmt::debug!("PPS: waiting for response");
    }

    /// Process a received byte
    ///
    /// Returns the new state after processing.
    /// When state becomes Done, check result with `result()`.
    /// When state becomes Failed, the negotiation failed.
    pub fn process_byte(&mut self, byte: u8) -> PpsState {
        defmt::trace!("PPS: state={:?} rx=0x{:02X}", self.state, byte);

        match self.state {
            PpsState::WaitPpsx => {
                // Wait for PPSS (0xFF)
                if byte == 0xFF {
                    self.rx_buf[self.rx_len] = byte;
                    self.rx_len += 1;
                    self.state = PpsState::WaitPps0;
                } else {
                    defmt::warn!("PPS: unexpected PPSS=0x{:02X}, expected 0xFF", byte);
                    self.state = PpsState::Failed;
                }
            }
            PpsState::WaitPps0 => {
                // PPS0 tells us which PPSx bytes follow
                self.rx_buf[self.rx_len] = byte;
                self.rx_len += 1;
                self.pps0_recv = byte;

                // Check which parameter bytes are present
                if (byte & 0x10) != 0 {
                    // PPS1 present
                    self.state = PpsState::WaitPps1;
                } else if (byte & 0x20) != 0 {
                    // PPS2 present (we don't use it, but must receive)
                    self.state = PpsState::WaitPps2;
                } else if (byte & 0x40) != 0 {
                    // PPS3 present (reserved)
                    self.state = PpsState::WaitPps3;
                } else {
                    // No parameter bytes, wait for PCK
                    self.state = PpsState::WaitPck;
                }
            }
            PpsState::WaitPps1 => {
                self.rx_buf[self.rx_len] = byte;
                self.rx_len += 1;

                if (self.pps0_recv & 0x20) != 0 {
                    self.state = PpsState::WaitPps2;
                } else if (self.pps0_recv & 0x40) != 0 {
                    self.state = PpsState::WaitPps3;
                } else {
                    self.state = PpsState::WaitPck;
                }
            }
            PpsState::WaitPps2 => {
                self.rx_buf[self.rx_len] = byte;
                self.rx_len += 1;

                if (self.pps0_recv & 0x40) != 0 {
                    self.state = PpsState::WaitPps3;
                } else {
                    self.state = PpsState::WaitPck;
                }
            }
            PpsState::WaitPps3 => {
                self.rx_buf[self.rx_len] = byte;
                self.rx_len += 1;
                self.state = PpsState::WaitPck;
            }
            PpsState::WaitPck => {
                // Final byte - checksum
                self.rx_buf[self.rx_len] = byte;
                self.rx_len += 1;

                // Verify checksum
                let mut computed_pck = 0u8;
                for i in 0..self.rx_len - 1 {
                    computed_pck ^= self.rx_buf[i];
                }

                if computed_pck != byte {
                    defmt::warn!(
                        "PPS: PCK mismatch computed=0x{:02X} received=0x{:02X}",
                        computed_pck,
                        byte
                    );
                    self.state = PpsState::Failed;
                } else {
                    // Verify response matches request
                    self.verify_response();
                }
            }
            _ => {
                defmt::warn!("PPS: unexpected byte in state {:?}", self.state);
            }
        }

        self.state
    }

    /// Verify response matches request
    fn verify_response(&mut self) {
        if self.rx_len != self.tx_len {
            defmt::warn!("PPS: length mismatch tx={} rx={}", self.tx_len, self.rx_len);
            self.state = PpsState::Failed;
            return;
        }

        for i in 0..self.tx_len {
            if self.rx_buf[i] != self.tx_buf[i] {
                defmt::warn!(
                    "PPS: byte {} mismatch tx=0x{:02X} rx=0x{:02X}",
                    i,
                    self.tx_buf[i],
                    self.rx_buf[i]
                );
                self.state = PpsState::Failed;
                return;
            }
        }

        defmt::info!("PPS: negotiation successful");
        self.state = PpsState::Done;
    }

    /// Get negotiation result (only valid when state is Done or Failed)
    pub fn result(&self) -> PpsResult {
        match self.state {
            PpsState::Done => PpsResult::Success,
            PpsState::Failed => PpsResult::Mismatch,
            _ => PpsResult::Error,
        }
    }

    /// Mark as timeout (card didn't respond)
    pub fn set_timeout(&mut self) {
        defmt::warn!("PPS: timeout - card did not respond");
        self.state = PpsState::Failed;
    }

    /// Reset the state machine for reuse
    pub fn reset(&mut self) {
        self.state = PpsState::Init;
        self.tx_len = 0;
        self.rx_len = 0;
        self.pps0_recv = 0;
    }
}

/// Fi values from TA1 upper nibble (ISO 7816-3 Table 7)
pub fn fi_from_ta1(ta1: u8) -> u16 {
    const FI_TABLE: [u16; 16] = [
        0, 372, 558, 744, 1116, 1488, 1860, 0, 0, 512, 768, 1024, 1536, 2048, 0, 0,
    ];
    let idx = (ta1 >> 4) as usize;
    if idx < FI_TABLE.len() && FI_TABLE[idx] != 0 {
        FI_TABLE[idx]
    } else {
        372 // Default
    }
}

/// Di values from TA1 lower nibble (ISO 7816-3 Table 8)
pub fn di_from_ta1(ta1: u8) -> u8 {
    const DI_TABLE: [u8; 16] = [0, 1, 2, 4, 8, 16, 32, 64, 12, 20, 0, 0, 0, 0, 0, 0];
    let idx = (ta1 & 0x0F) as usize;
    if idx < DI_TABLE.len() && DI_TABLE[idx] != 0 {
        DI_TABLE[idx]
    } else {
        1 // Default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_request() {
        let mut fsm = PpsFsm::new();
        let req = fsm.build_request(0, 0x11); // T=0, Fi=372, Di=1

        // [0xFF, PPS0, PPS1, PCK]
        // PPS0 = 0x10 | 0 = 0x10 (PPS1 present, T=0)
        // PPS1 = 0x11
        // PCK = 0xFF ^ 0x10 ^ 0x11 = 0xDE
        assert_eq!(req, &[0xFF, 0x10, 0x11, 0xDE]);
        assert_eq!(fsm.state(), PpsState::TxRequest);
    }

    #[test]
    fn test_successful_negotiation() {
        let mut fsm = PpsFsm::new();
        let req = fsm.build_request(0, 0x11);

        fsm.start_response();

        // Card echoes the request
        for &byte in req {
            let state = fsm.process_byte(byte);
            if byte == req[req.len() - 1] {
                assert_eq!(state, PpsState::Done);
            }
        }

        assert_eq!(fsm.result(), PpsResult::Success);
    }

    #[test]
    fn test_mismatch_response() {
        let mut fsm = PpsFsm::new();
        fsm.build_request(0, 0x11);

        fsm.start_response();

        // Card sends different response
        fsm.process_byte(0xFF); // PPSS ok
        fsm.process_byte(0x10); // PPS0 ok
        fsm.process_byte(0x13); // PPS1 different (0x11 expected)
        fsm.process_byte(0xFF ^ 0x10 ^ 0x13); // PCK for wrong response

        assert_eq!(fsm.state(), PpsState::Failed);
        assert_eq!(fsm.result(), PpsResult::Mismatch);
    }

    #[test]
    fn test_invalid_ppss() {
        let mut fsm = PpsFsm::new();
        fsm.build_request(0, 0x11);
        fsm.start_response();

        // Card sends invalid PPSS
        let state = fsm.process_byte(0x00);
        assert_eq!(state, PpsState::Failed);
    }

    #[test]
    fn test_fi_di_tables() {
        // Default values
        assert_eq!(fi_from_ta1(0x11), 372);
        assert_eq!(di_from_ta1(0x11), 1);

        // Higher speed
        assert_eq!(fi_from_ta1(0x94), 512); // Fi=512 (9 << 4)
        assert_eq!(di_from_ta1(0x94), 4); // Di=4 (4)

        // Fi=744, Di=8
        assert_eq!(fi_from_ta1(0x38), 744);
        assert_eq!(di_from_ta1(0x38), 8);
    }
}
