//! M5Stack Atom Matrix 5×5 WS2812C LED diagnostic display.
//!
//! Drives the onboard 25-LED RGB matrix on GPIO27 via the ESP32 RMT peripheral.
//! Each [`LedState`] maps to a distinct visual pattern for at-a-glance diagnostics.

#[cfg(feature = "backend-mfrc522")]
use esp_idf_hal::rmt::config::{TransmitConfig, TxChannelConfig};
#[cfg(feature = "backend-mfrc522")]
use esp_idf_hal::rmt::encoder::{BytesEncoder, BytesEncoderConfig};
#[cfg(feature = "backend-mfrc522")]
use esp_idf_hal::rmt::{PinState, Pulse, Symbol, TxChannelDriver};

#[cfg(feature = "backend-mfrc522")]
pub const LED_COUNT: usize = 25;

/// Max LED brightness (M5Stack recommends ≤20 to avoid damage).
#[cfg(feature = "backend-mfrc522")]
const BRIGHT: u8 = 15;

#[cfg(feature = "backend-mfrc522")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedState {
    Init,
    Ready,
    CardPresent,
    TxRx,
    Error,
    Off,
}

#[cfg(feature = "backend-mfrc522")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[cfg(feature = "backend-mfrc522")]
impl Rgb {
    const fn black() -> Self {
        Self(0, 0, 0)
    }
}

#[cfg(feature = "backend-mfrc522")]
pub struct LedStatus {
    tx: TxChannelDriver<'static>,
    encoder: BytesEncoder,
    current_state: LedState,
}

#[cfg(feature = "backend-mfrc522")]
impl LedStatus {
    pub fn new() -> Self {
        let peripherals = unsafe { esp_idf_hal::peripherals::Peripherals::steal() };

        let config = TxChannelConfig::default();

        let tx = TxChannelDriver::new(peripherals.pins.gpio27, &config).expect("LED RMT TX init");

        let res = esp_idf_hal::units::Hertz(1_000_000);
        let bit0 = Symbol::new(
            Pulse::new_with_duration(res, PinState::High, core::time::Duration::from_nanos(400))
                .expect("T0H"),
            Pulse::new_with_duration(res, PinState::Low, core::time::Duration::from_nanos(850))
                .expect("T0L"),
        );
        let bit1 = Symbol::new(
            Pulse::new_with_duration(res, PinState::High, core::time::Duration::from_nanos(800))
                .expect("T1H"),
            Pulse::new_with_duration(res, PinState::Low, core::time::Duration::from_nanos(450))
                .expect("T1L"),
        );

        let encoder = BytesEncoder::with_config(&BytesEncoderConfig {
            bit0,
            bit1,
            msb_first: true,
            ..Default::default()
        })
        .expect("LED bytes encoder init");

        let mut led = Self {
            tx,
            encoder,
            current_state: LedState::Off,
        };
        led.render(LedState::Init);
        led
    }

    pub fn set_state(&mut self, state: LedState) {
        if state != self.current_state {
            log::info!(
                "LED: {}",
                match state {
                    LedState::Init => "Init (amber center)",
                    LedState::Ready => "Ready (green center)",
                    LedState::CardPresent => "CardPresent (blue ring)",
                    LedState::TxRx => "TxRx (yellow flash)",
                    LedState::Error => "Error (red X)",
                    LedState::Off => "Off",
                }
            );
            self.render(state);
            self.current_state = state;
        }
    }

    /// Blink a state pattern on/off `count` times, then leave it on.
    /// Blocks for `count * (on_ms + off_ms)` milliseconds.
    pub fn blink_state(&mut self, state: LedState, count: u32, on_ms: u32, off_ms: u32) {
        log::info!(
            "LED blink: {} × {} ({}ms on / {}ms off)",
            match state {
                LedState::Init => "Init (amber center)",
                LedState::Ready => "Ready (green center)",
                LedState::CardPresent => "CardPresent (blue ring)",
                LedState::TxRx => "TxRx (yellow flash)",
                LedState::Error => "Error (red X)",
                LedState::Off => "Off",
            },
            count,
            on_ms,
            off_ms
        );
        for _ in 0..count {
            self.render(state);
            esp_idf_hal::delay::FreeRtos::delay_ms(on_ms);
            self.render(LedState::Off);
            esp_idf_hal::delay::FreeRtos::delay_ms(off_ms);
        }
        self.render(state);
        self.current_state = state;
    }

    pub fn state(&self) -> LedState {
        self.current_state
    }

    fn render(&mut self, state: LedState) {
        let pixels = match state {
            LedState::Init => Self::pattern_center(Rgb(BRIGHT, BRIGHT / 2, 0)),
            LedState::Ready => Self::pattern_center(Rgb(0, BRIGHT, 0)),
            LedState::CardPresent => Self::pattern_ring(Rgb(0, 0, BRIGHT)),
            LedState::TxRx => Self::pattern_center(Rgb(BRIGHT, BRIGHT, 0)),
            LedState::Error => Self::pattern_error(),
            LedState::Off => Self::pattern_off(),
        };

        let mut buf = [0u8; LED_COUNT * 3];
        for (i, px) in pixels.iter().enumerate() {
            // WS2812 expects GRB byte order
            buf[i * 3] = px.1;
            buf[i * 3 + 1] = px.0;
            buf[i * 3 + 2] = px.2;
        }

        if let Err(e) = self
            .tx
            .send_and_wait(&mut self.encoder, &buf, &TransmitConfig::default())
        {
            log::error!("LED write failed: {:?}", e);
        }
    }

    // ---- Pattern generators ----

    /// Single center pixel (position 12 in row-major 5×5).
    /// Pattern:
    /// ```text
    /// . . . . .
    /// . . . . .
    /// . . * . .
    /// . . . . .
    /// . . . . .
    /// ```
    fn pattern_center(color: Rgb) -> [Rgb; LED_COUNT] {
        let mut pixels = [Rgb::black(); LED_COUNT];
        pixels[12] = color;
        pixels
    }

    /// Ring of 8 pixels around the center.
    /// Pattern:
    /// ```text
    /// . * * * .
    /// * . . . *
    /// * . . . *
    /// * . . . *
    /// . * * * .
    /// ```
    fn pattern_ring(color: Rgb) -> [Rgb; LED_COUNT] {
        let mut pixels = [Rgb::black(); LED_COUNT];
        for &idx in &[1, 2, 3, 5, 9, 10, 14, 15, 19, 21, 22, 23] {
            pixels[idx] = color;
        }
        pixels
    }

    /// Red X pattern for errors.
    /// Pattern:
    /// ```text
    /// * . . . *
    /// . * . * .
    /// . . * . .
    /// . * . * .
    /// * . . . *
    /// ```
    fn pattern_error() -> [Rgb; LED_COUNT] {
        let red = Rgb(BRIGHT, 0, 0);
        let mut pixels = [Rgb::black(); LED_COUNT];
        for &idx in &[0, 4, 6, 8, 12, 16, 18, 20, 24] {
            pixels[idx] = red;
        }
        pixels
    }

    fn pattern_off() -> [Rgb; LED_COUNT] {
        [Rgb::black(); LED_COUNT]
    }
}

#[cfg(feature = "backend-mfrc522")]
impl Default for LedStatus {
    fn default() -> Self {
        Self::new()
    }
}

// ---- Host-testable stubs (non-xtensa) ----

#[cfg(all(test, feature = "backend-mfrc522"))]
mod tests {
    use super::*;

    #[test]
    fn test_led_state_equality() {
        assert_eq!(LedState::Ready, LedState::Ready);
        assert_ne!(LedState::Ready, LedState::Error);
        assert_ne!(LedState::TxRx, LedState::CardPresent);
    }

    #[test]
    fn test_led_state_variants() {
        // Ensure all variants compile and are distinct
        let states = [
            LedState::Init,
            LedState::Ready,
            LedState::CardPresent,
            LedState::TxRx,
            LedState::Error,
            LedState::Off,
        ];
        for (i, a) in states.iter().enumerate() {
            for (j, b) in states.iter().enumerate() {
                assert_eq!(
                    a == b,
                    i == j,
                    "variant {:?} == {:?} should be {}",
                    a,
                    b,
                    i == j
                );
            }
        }
    }

    #[test]
    fn test_pattern_center_only_position_12_lit() {
        let green = Rgb(0, 15, 0);
        let pixels = LedStatus::pattern_center(green);
        assert_eq!(pixels[12], green);
        assert_eq!(pixels[0], Rgb::black());
        assert_eq!(pixels[24], Rgb::black());
    }

    #[test]
    fn test_pattern_ring_edges_only() {
        let blue = Rgb(0, 0, 15);
        let pixels = LedStatus::pattern_ring(blue);
        for &idx in &[1, 2, 3, 5, 9, 10, 14, 15, 19, 21, 22, 23] {
            assert_eq!(pixels[idx], blue, "ring pixel {} should be blue", idx);
        }
        assert_eq!(pixels[12], Rgb::black());
        for &idx in &[0, 4, 20, 24] {
            assert_eq!(pixels[idx], Rgb::black(), "corner {} should be off", idx);
        }
    }

    #[test]
    fn test_pattern_error_diagonals_only() {
        let pixels = LedStatus::pattern_error();
        let red = Rgb(BRIGHT, 0, 0);
        for &idx in &[0, 4, 6, 8, 12, 16, 18, 20, 24] {
            assert_eq!(pixels[idx], red, "X pixel {} should be red", idx);
        }
        for &idx in &[1, 2, 3, 5, 7, 9, 10, 11, 13, 14] {
            assert_eq!(pixels[idx], Rgb::black(), "pixel {} should be off", idx);
        }
    }

    #[test]
    fn test_pattern_off_all_black() {
        let pixels = LedStatus::pattern_off();
        for (i, &p) in pixels.iter().enumerate() {
            assert_eq!(p, Rgb::black(), "pixel {} should be off", i);
        }
    }

    #[test]
    fn test_led_count() {
        assert_eq!(LED_COUNT, 25);
    }

    #[test]
    fn test_set_state_skip_unchanged() {
        let mut led = LedStatus::new();
        assert_eq!(led.state(), LedState::Init);
        led.set_state(LedState::Init);
        assert_eq!(led.state(), LedState::Init);
    }

    #[test]
    fn test_state_transitions() {
        let mut led = LedStatus::new();
        assert_eq!(led.state(), LedState::Init);

        led.set_state(LedState::Ready);
        assert_eq!(led.state(), LedState::Ready);

        led.set_state(LedState::CardPresent);
        assert_eq!(led.state(), LedState::CardPresent);

        led.set_state(LedState::Ready);
        assert_eq!(led.state(), LedState::Ready);

        led.set_state(LedState::Error);
        assert_eq!(led.state(), LedState::Error);

        led.set_state(LedState::Off);
        assert_eq!(led.state(), LedState::Off);
    }
}
