//! PIN entry state machine
//!
//! This module implements the state machine for PIN entry operations.
//! The state machine handles transitions between:
//! - Idle (no PIN entry active)
//! - WaitingForPin (UI displayed, waiting for user input)
//! - Completed (PIN entered, ready to send APDU)
//! - Cancelled (user cancelled)
//! - Timeout (time limit exceeded)
#![allow(dead_code)]

use crate::pinpad::{PinBuffer, PinResult, PinVerifyParams};

/// PIN entry state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PinEntryState {
    /// No PIN entry in progress
    #[default]
    Idle,
    /// PIN entry active, waiting for user input
    WaitingForPin,
    /// PIN entry completed successfully (user pressed OK)
    Completed,
    /// User cancelled PIN entry
    Cancelled,
    /// PIN entry timed out
    Timeout,
    /// Invalid PIN length entered
    InvalidLength,
}

/// PIN entry context - holds all state for a PIN entry session
#[derive(Debug)]
pub struct PinEntryContext {
    /// Current state
    pub state: PinEntryState,
    /// PIN buffer
    pub buffer: PinBuffer,
    /// Parameters from CCID Secure command
    pub params: PinVerifyParams,
    /// Start time (in system ticks)
    pub start_ticks: u32,
    /// Currently pressed button (for UI highlighting)
    pub pressed_button: Option<u8>,
}

impl PinEntryContext {
    /// Create a new PIN entry context
    pub fn new(params: PinVerifyParams) -> Self {
        Self {
            state: PinEntryState::Idle,
            buffer: PinBuffer::new(params.max_len as usize),
            params,
            start_ticks: 0,
            pressed_button: None,
        }
    }

    /// Start PIN entry
    pub fn start(&mut self, current_ticks: u32) {
        self.state = PinEntryState::WaitingForPin;
        self.buffer.clear();
        self.start_ticks = current_ticks;
        self.pressed_button = None;
    }

    /// Add a digit to the PIN
    pub fn add_digit(&mut self, digit: u8) -> bool {
        if self.state != PinEntryState::WaitingForPin {
            return false;
        }
        self.buffer.push(digit)
    }

    /// Remove the last digit (backspace)
    pub fn backspace(&mut self) -> bool {
        if self.state != PinEntryState::WaitingForPin {
            return false;
        }
        self.buffer.pop()
    }

    /// Submit the PIN (user pressed OK)
    pub fn submit(&mut self) -> PinResult {
        if self.state != PinEntryState::WaitingForPin {
            return PinResult::Cancelled;
        }

        // Validate PIN length
        if !self.buffer.has_minimum(self.params.min_len as usize) {
            self.state = PinEntryState::InvalidLength;
            return PinResult::InvalidLength;
        }

        self.state = PinEntryState::Completed;
        PinResult::Success
    }

    /// Cancel PIN entry
    pub fn cancel(&mut self) {
        self.state = PinEntryState::Cancelled;
        self.buffer.clear();
    }

    /// Check for timeout
    ///
    /// # Arguments
    /// * `current_ticks` - Current system tick count
    /// * `ticks_per_second` - How many ticks per second
    ///
    /// # Returns
    /// True if timeout occurred, false otherwise
    pub fn check_timeout(&mut self, current_ticks: u32, ticks_per_second: u32) -> bool {
        if self.state != PinEntryState::WaitingForPin {
            return false;
        }

        // Timeout of 0 means no timeout
        if self.params.timeout_secs == 0 {
            return false;
        }

        let elapsed_ticks = current_ticks.wrapping_sub(self.start_ticks);
        let elapsed_secs = elapsed_ticks / ticks_per_second;

        if elapsed_secs >= self.params.timeout_secs as u32 {
            self.state = PinEntryState::Timeout;
            self.buffer.clear();
            return true;
        }

        false
    }

    /// Check if PIN entry is active
    pub fn is_active(&self) -> bool {
        matches!(self.state, PinEntryState::WaitingForPin)
    }

    /// Check if PIN entry is complete (success or failure)
    pub fn is_complete(&self) -> bool {
        matches!(
            self.state,
            PinEntryState::Completed
                | PinEntryState::Cancelled
                | PinEntryState::Timeout
                | PinEntryState::InvalidLength
        )
    }

    /// Get the result of PIN entry
    pub fn result(&self) -> Option<PinResult> {
        match self.state {
            PinEntryState::Completed => Some(PinResult::Success),
            PinEntryState::Cancelled => Some(PinResult::Cancelled),
            PinEntryState::Timeout => Some(PinResult::Timeout),
            PinEntryState::InvalidLength => Some(PinResult::InvalidLength),
            _ => None,
        }
    }

    /// Reset the context for reuse
    pub fn reset(&mut self) {
        self.state = PinEntryState::Idle;
        self.buffer.clear();
        self.start_ticks = 0;
        self.pressed_button = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_machine_start() {
        let params = PinVerifyParams::default();
        let mut ctx = PinEntryContext::new(params);

        assert_eq!(ctx.state, PinEntryState::Idle);
        assert!(!ctx.is_active());

        ctx.start(0);
        assert_eq!(ctx.state, PinEntryState::WaitingForPin);
        assert!(ctx.is_active());
    }

    #[test]
    fn test_add_digit() {
        let params = PinVerifyParams::default();
        let mut ctx = PinEntryContext::new(params);
        ctx.start(0);

        assert!(ctx.add_digit(1));
        assert!(ctx.add_digit(2));
        assert!(ctx.add_digit(3));
        assert_eq!(ctx.buffer.len(), 3);
    }

    #[test]
    fn test_add_digit_when_not_active() {
        let params = PinVerifyParams::default();
        let mut ctx = PinEntryContext::new(params);
        // Don't call start()

        assert!(!ctx.add_digit(1));
        assert_eq!(ctx.buffer.len(), 0);
    }

    #[test]
    fn test_backspace() {
        let params = PinVerifyParams::default();
        let mut ctx = PinEntryContext::new(params);
        ctx.start(0);

        ctx.add_digit(1);
        ctx.add_digit(2);
        assert_eq!(ctx.buffer.len(), 2);

        assert!(ctx.backspace());
        assert_eq!(ctx.buffer.len(), 1);

        let ascii = ctx.buffer.to_ascii();
        assert_eq!(ascii[0], b'1');
    }

    #[test]
    fn test_submit_too_short() {
        let params = PinVerifyParams {
            min_len: 6,
            max_len: 8,
            ..PinVerifyParams::default()
        };
        let mut ctx = PinEntryContext::new(params);
        ctx.start(0);

        // Enter only 3 digits
        ctx.add_digit(1);
        ctx.add_digit(2);
        ctx.add_digit(3);

        let result = ctx.submit();
        assert_eq!(result, PinResult::InvalidLength);
        assert_eq!(ctx.state, PinEntryState::InvalidLength);
    }

    #[test]
    fn test_submit_success() {
        let params = PinVerifyParams {
            min_len: 6,
            max_len: 8,
            ..PinVerifyParams::default()
        };
        let mut ctx = PinEntryContext::new(params);
        ctx.start(0);

        // Enter 6 digits (minimum)
        for d in [1, 2, 3, 4, 5, 6] {
            ctx.add_digit(d);
        }

        let result = ctx.submit();
        assert_eq!(result, PinResult::Success);
        assert_eq!(ctx.state, PinEntryState::Completed);
    }

    #[test]
    fn test_cancel() {
        let params = PinVerifyParams::default();
        let mut ctx = PinEntryContext::new(params);
        ctx.start(0);

        ctx.add_digit(1);
        ctx.add_digit(2);

        ctx.cancel();
        assert_eq!(ctx.state, PinEntryState::Cancelled);
        assert_eq!(ctx.buffer.len(), 0); // Buffer should be cleared
    }

    #[test]
    fn test_timeout() {
        let params = PinVerifyParams {
            timeout_secs: 10,
            ..PinVerifyParams::default()
        };
        let mut ctx = PinEntryContext::new(params);
        ctx.start(0);

        // Check timeout at 5 seconds (should not timeout)
        assert!(!ctx.check_timeout(5000, 1000));
        assert_eq!(ctx.state, PinEntryState::WaitingForPin);

        // Check timeout at 10 seconds (should timeout)
        assert!(ctx.check_timeout(10000, 1000));
        assert_eq!(ctx.state, PinEntryState::Timeout);
    }

    #[test]
    fn test_no_timeout() {
        let params = PinVerifyParams {
            timeout_secs: 0, // No timeout
            ..PinVerifyParams::default()
        };
        let mut ctx = PinEntryContext::new(params);
        ctx.start(0);

        // Should never timeout when timeout_secs = 0
        assert!(!ctx.check_timeout(100000, 1000));
        assert_eq!(ctx.state, PinEntryState::WaitingForPin);
    }

    #[test]
    fn test_result() {
        let params = PinVerifyParams::default();

        let mut ctx = PinEntryContext::new(params);
        ctx.start(0);
        assert!(ctx.result().is_none());

        ctx.cancel();
        assert_eq!(ctx.result(), Some(PinResult::Cancelled));
    }

    #[test]
    fn test_reset() {
        let params = PinVerifyParams::default();
        let mut ctx = PinEntryContext::new(params);

        ctx.start(0);
        ctx.add_digit(1);
        ctx.add_digit(2);
        ctx.cancel();

        ctx.reset();
        assert_eq!(ctx.state, PinEntryState::Idle);
        assert_eq!(ctx.buffer.len(), 0);
        assert_eq!(ctx.start_ticks, 0);
    }

    #[test]
    fn test_max_pin_length() {
        let params = PinVerifyParams {
            min_len: 6,
            max_len: 8,
            ..PinVerifyParams::default()
        };
        let mut ctx = PinEntryContext::new(params);
        ctx.start(0);

        // Add 8 digits (max)
        for d in [1, 2, 3, 4, 5, 6, 7, 8] {
            assert!(ctx.add_digit(d));
        }

        // 9th digit should fail
        assert!(!ctx.add_digit(9));
        assert_eq!(ctx.buffer.len(), 8);
    }

    #[test]
    fn test_is_complete() {
        let params = PinVerifyParams::default();
        let mut ctx = PinEntryContext::new(params);

        assert!(!ctx.is_complete());

        ctx.start(0);
        assert!(!ctx.is_complete());

        ctx.cancel();
        assert!(ctx.is_complete());

        ctx.reset();
        ctx.start(0);
        for d in [1, 2, 3, 4, 5, 6] {
            ctx.add_digit(d);
        }
        ctx.submit();
        assert!(ctx.is_complete());
    }

    #[test]
    fn test_double_start() {
        let params = PinVerifyParams::default();
        let mut ctx = PinEntryContext::new(params);

        ctx.start(0);
        ctx.add_digit(1);
        ctx.add_digit(2);

        // Start again should reset
        ctx.start(100);
        assert_eq!(ctx.state, PinEntryState::WaitingForPin);
        assert_eq!(ctx.buffer.len(), 0);
        assert_eq!(ctx.start_ticks, 100);
    }

    #[test]
    fn test_operations_after_complete() {
        let params = PinVerifyParams {
            min_len: 6,
            max_len: 8,
            ..PinVerifyParams::default()
        };
        let mut ctx = PinEntryContext::new(params);
        ctx.start(0);

        for d in [1, 2, 3, 4, 5, 6] {
            ctx.add_digit(d);
        }
        ctx.submit();

        // Operations after complete should fail
        assert!(!ctx.add_digit(7));
        assert!(!ctx.backspace());
        assert!(!ctx.check_timeout(5000, 1000));
    }

    #[test]
    fn test_submit_empty() {
        let params = PinVerifyParams {
            min_len: 1,
            max_len: 8,
            ..PinVerifyParams::default()
        };
        let mut ctx = PinEntryContext::new(params);
        ctx.start(0);

        // Submit empty buffer
        let result = ctx.submit();
        assert_eq!(result, PinResult::InvalidLength);
    }

    #[test]
    fn test_backspace_empty() {
        let params = PinVerifyParams::default();
        let mut ctx = PinEntryContext::new(params);
        ctx.start(0);

        // Backspace on empty buffer should fail
        assert!(!ctx.backspace());
    }
}
