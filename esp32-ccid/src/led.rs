#[cfg(feature = "backend-mfrc522")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedState {
    Ready,
    CardPresent,
    Error,
    Off,
}

#[cfg(feature = "backend-mfrc522")]
pub struct LedStatus;

#[cfg(feature = "backend-mfrc522")]
impl LedStatus {
    pub fn new() -> Self {
        Self
    }

    pub fn set_state(&mut self, state: LedState) {
        log::info!(
            "LED: {}",
            match state {
                LedState::Ready => "Ready",
                LedState::CardPresent => "CardPresent",
                LedState::Error => "Error",
                LedState::Off => "Off",
            }
        );
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
    fn test_led_state_equality() {
        assert_eq!(LedState::Ready, LedState::Ready);
        assert_ne!(LedState::Ready, LedState::Error);
    }

    #[test]
    #[cfg(feature = "backend-mfrc522")]
    fn test_led_set_state() {
        let mut led = LedStatus::new();
        led.set_state(LedState::Ready);
        led.set_state(LedState::CardPresent);
        led.set_state(LedState::Error);
        led.set_state(LedState::Off);
    }
}
