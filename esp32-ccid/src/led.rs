//! LED status indicator for M5Stack Atom Matrix (WS2812 5x5 matrix).
//!
//! This module provides LED state management with color-coded status:
//! - Ready: Green
//! - Card Present: Blue
//! - Error: Red
//! - Off: Black
//!
//! Current implementation: Log-only stub (LED state logged to serial).
//! Future: Real WS2812 control via esp-idf-hal RMT.

#[cfg(feature = "backend-mfrc522")]
/// LED state with color mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedState {
    /// Ready state - Green LED
    Ready,
    /// Card present - Blue LED
    CardPresent,
    /// Error state - Red LED
    Error,
    /// LED off - Black
    Off,
}

#[cfg(feature = "backend-mfrc522")]
/// LED status controller for M5Stack Atom Matrix.
///
/// Manages the 5x5 WS2812 LED matrix status display.
/// Currently logs state to serial for debugging.
pub struct LedStatus;

#[cfg(feature = "backend-mfrc522")]
impl LedStatus {
    /// Create a new LED status controller.
    ///
    /// # Note
    /// This is a log-only stub implementation.
    /// Future versions will initialize RMT and GPIO for WS2812 control.
    pub fn new() -> Self {
        Self
    }

    /// Set the LED state.
    ///
    /// # Arguments
    /// * `state` - The desired LED state (Ready, CardPresent, Error, Off)
    ///
    /// # Note
    /// Currently logs the state to serial. Future versions will drive the
    /// WS2812 LED matrix via esp-idf-hal RMT.
    pub fn set_state(&mut self, state: LedState) {
        let name = match state {
            LedState::Ready => "Ready (green)",
            LedState::CardPresent => "CardPresent (blue)",
            LedState::Error => "Error (red)",
            LedState::Off => "Off",
        };
        log::info!("LED: {}", name);
    }
}

#[cfg(feature = "backend-mfrc522")]
impl Default for LedStatus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "backend-mfrc522")]
    fn test_led_state_display() {
        let mut led = LedStatus::new();

        // Should log "LED: Ready (green)"
        led.set_state(LedState::Ready);
        led.set_state(LedState::CardPresent);
        led.set_state(LedState::Error);
        led.set_state(LedState::Off);
    }

    #[test]
    #[cfg(feature = "backend-mfrc522")]
    fn test_led_state_equality() {
        assert_eq!(LedState::Ready, LedState::Ready);
        assert_ne!(LedState::Ready, LedState::Error);
    }

    #[test]
    #[cfg(feature = "backend-mfrc522")]
    fn test_led_status_default() {
        let mut led = LedStatus::default();
        led.set_state(LedState::Off);
    }
}
