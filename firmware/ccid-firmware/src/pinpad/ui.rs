//! Pinpad UI implementation using embedded-graphics
//!
//! This module draws a numeric keypad on the STM32F469-DISCO display
//! and handles touch input for PIN entry.
//!
//! Layout (480x800 portrait):
//! - Title area: Y 0-80
//! - PIN display: Y 80-180 (masked with ****)
//! - Keypad: Y 200-700
//! - Status: Y 720-800

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
    text::Text,
    Drawable,
};

/// Pinpad colors
pub const COLOR_BG: Rgb565 = Rgb565::BLACK;
pub const COLOR_BUTTON: Rgb565 = Rgb565::CSS_DARK_GRAY;
pub const COLOR_BUTTON_HOVER: Rgb565 = Rgb565::CSS_SLATE_GRAY;
pub const COLOR_BUTTON_OK: Rgb565 = Rgb565::new(0x1F >> 3, 0x8B >> 2, 0x00 >> 3); // Green
pub const COLOR_BUTTON_CANCEL: Rgb565 = Rgb565::new(0xDC >> 3, 0x14 >> 2, 0x3C >> 3); // Red
pub const COLOR_TEXT: Rgb565 = Rgb565::WHITE;
pub const COLOR_ACCENT: Rgb565 = Rgb565::CSS_CYAN;

/// Screen dimensions
pub const SCREEN_WIDTH: u32 = 480;
pub const SCREEN_HEIGHT: u32 = 800;

/// Button dimensions
pub const BUTTON_SIZE: u32 = 100;
pub const BUTTON_SPACING: u32 = 20;
pub const KEYPAD_START_Y: i32 = 200;
pub const KEYPAD_START_X: i32 = 50;

/// Button identifiers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonId {
    /// Digit button (0-9)
    Digit(u8),
    /// OK/Confirm button
    Ok,
    /// Cancel button
    Cancel,
    /// Backspace button
    Backspace,
    /// No button hit
    None,
}

impl Default for ButtonId {
    fn default() -> Self {
        ButtonId::None
    }
}

/// Button definition for keypad layout
#[derive(Debug, Clone, Copy)]
pub struct Button {
    /// Button identifier
    pub id: ButtonId,
    /// Bounding rectangle
    pub bounds: Rectangle,
    /// Label text
    pub label: &'static str,
    /// Fill color
    pub color: Rgb565,
}

impl Button {
    /// Create a new button
    pub fn new(id: ButtonId, x: i32, y: i32, label: &'static str, color: Rgb565) -> Self {
        let top_left = Point::new(x, y);
        let size = Size::new(BUTTON_SIZE, BUTTON_SIZE);
        Self {
            id,
            bounds: Rectangle::new(top_left, size),
            label,
            color,
        }
    }

    /// Check if a point is inside this button
    pub fn contains(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

/// Keypad layout with all buttons
#[derive(Debug, Clone, Copy)]
pub struct Keypad {
    /// All buttons (13 total: 0-9, OK, Cancel, Backspace)
    buttons: [Button; 13],
}

impl Keypad {
    /// Create a new keypad with standard layout
    ///
    /// Layout:
    /// ```text
    ///   [1] [2] [3]
    ///   [4] [5] [6]
    ///   [7] [8] [9]
    ///  [<] [0] [OK]
    ///   [X] (centered below)
    /// ```
    pub fn new() -> Self {
        let step = BUTTON_SIZE as i32 + BUTTON_SPACING as i32;
        let bottom_y = KEYPAD_START_Y + 3 * step;
        let cancel_y = bottom_y + step;
        let cancel_x = KEYPAD_START_X + step;
        let buttons = [
            Button::new(
                ButtonId::Digit(1),
                KEYPAD_START_X,
                KEYPAD_START_Y,
                "1",
                COLOR_BUTTON,
            ),
            Button::new(
                ButtonId::Digit(2),
                KEYPAD_START_X + step,
                KEYPAD_START_Y,
                "2",
                COLOR_BUTTON,
            ),
            Button::new(
                ButtonId::Digit(3),
                KEYPAD_START_X + 2 * step,
                KEYPAD_START_Y,
                "3",
                COLOR_BUTTON,
            ),
            Button::new(
                ButtonId::Digit(4),
                KEYPAD_START_X,
                KEYPAD_START_Y + step,
                "4",
                COLOR_BUTTON,
            ),
            Button::new(
                ButtonId::Digit(5),
                KEYPAD_START_X + step,
                KEYPAD_START_Y + step,
                "5",
                COLOR_BUTTON,
            ),
            Button::new(
                ButtonId::Digit(6),
                KEYPAD_START_X + 2 * step,
                KEYPAD_START_Y + step,
                "6",
                COLOR_BUTTON,
            ),
            Button::new(
                ButtonId::Digit(7),
                KEYPAD_START_X,
                KEYPAD_START_Y + 2 * step,
                "7",
                COLOR_BUTTON,
            ),
            Button::new(
                ButtonId::Digit(8),
                KEYPAD_START_X + step,
                KEYPAD_START_Y + 2 * step,
                "8",
                COLOR_BUTTON,
            ),
            Button::new(
                ButtonId::Digit(9),
                KEYPAD_START_X + 2 * step,
                KEYPAD_START_Y + 2 * step,
                "9",
                COLOR_BUTTON,
            ),
            Button::new(
                ButtonId::Backspace,
                KEYPAD_START_X,
                bottom_y,
                "<",
                COLOR_BUTTON,
            ),
            Button::new(
                ButtonId::Digit(0),
                KEYPAD_START_X + step,
                bottom_y,
                "0",
                COLOR_BUTTON,
            ),
            Button::new(
                ButtonId::Ok,
                KEYPAD_START_X + 2 * step,
                bottom_y,
                "OK",
                COLOR_BUTTON_OK,
            ),
            Button::new(
                ButtonId::Cancel,
                cancel_x,
                cancel_y,
                "X",
                COLOR_BUTTON_CANCEL,
            ),
        ];

        Self { buttons }
    }

    /// Find which button contains the given point
    pub fn hit_test(&self, point: Point) -> ButtonId {
        for button in &self.buttons {
            if button.contains(point) {
                return button.id;
            }
        }
        ButtonId::None
    }

    /// Get all buttons
    pub fn buttons(&self) -> &[Button] {
        &self.buttons
    }

    /// Get a button by its ID
    pub fn get_button(&self, id: ButtonId) -> Option<&Button> {
        self.buttons.iter().find(|b| b.id == id)
    }
}

impl Default for Keypad {
    fn default() -> Self {
        Self::new()
    }
}

/// Draw the complete pinpad UI
///
/// # Arguments
/// * `display` - The draw target (framebuffer)
/// * `title` - Title text to display
/// * `pin_mask` - Masked PIN representation (e.g., "****")
/// * `keypad` - The keypad layout
/// * `status` - Optional status message
/// * `pressed_button` - Currently pressed button for highlighting
pub fn draw_pinpad<D>(
    display: &mut D,
    title: &str,
    pin_mask: &str,
    keypad: &Keypad,
    status: Option<&str>,
    pressed_button: Option<ButtonId>,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    // Clear screen
    display.clear(COLOR_BG)?;

    // Draw title
    let title_style = MonoTextStyle::new(&FONT_10X20, COLOR_TEXT);
    let title_x = (SCREEN_WIDTH as i32 / 2).saturating_sub((title.len() as i32 * 10) / 2);
    Text::new(title, Point::new(title_x, 50), title_style).draw(display)?;

    // Draw PIN mask (centered)
    let pin_style = MonoTextStyle::new(&FONT_10X20, COLOR_ACCENT);
    let pin_x = (SCREEN_WIDTH as i32 / 2).saturating_sub((pin_mask.len() as i32 * 10) / 2);
    Text::new(pin_mask, Point::new(pin_x, 130), pin_style).draw(display)?;

    // Draw keypad buttons
    for button in keypad.buttons() {
        let color = if pressed_button == Some(button.id) {
            COLOR_BUTTON_HOVER
        } else {
            button.color
        };

        // Draw button background
        let style = PrimitiveStyleBuilder::new()
            .fill_color(color)
            .stroke_color(COLOR_TEXT)
            .stroke_width(2)
            .build();

        button.bounds.into_styled(style).draw(display)?;

        // Draw button label (centered)
        let label_style = MonoTextStyle::new(&FONT_10X20, COLOR_TEXT);
        let label_x =
            button.bounds.top_left.x + (BUTTON_SIZE as i32 / 2) - (button.label.len() as i32 * 5);
        let label_y = button.bounds.top_left.y + (BUTTON_SIZE as i32 / 2) + 10;

        Text::new(button.label, Point::new(label_x, label_y), label_style).draw(display)?;
    }

    // Draw status message if any
    if let Some(status_text) = status {
        let status_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_ORANGE_RED);
        let status_x = (SCREEN_WIDTH as i32 / 2).saturating_sub(status_text.len() as i32 * 5);
        Text::new(status_text, Point::new(status_x, 750), status_style).draw(display)?;
    }

    Ok(())
}

/// Draw a simple PIN entry screen
///
/// Convenience function for initial testing
pub fn draw_simple_pin_screen<D>(
    display: &mut D,
    pin_len: usize,
    pin_type: &str,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let keypad = Keypad::new();
    // Since we can't use String in no_std easily, use a fixed buffer
    let mut mask_buf = [0u8; 8];
    for i in 0..pin_len.min(8) {
        mask_buf[i] = b'*';
    }
    let mask_str = core::str::from_utf8(&mask_buf[..pin_len.min(8)]).unwrap_or("****");

    let title = if pin_type == "Admin" {
        "Enter Admin PIN"
    } else {
        "Enter PIN"
    };

    draw_pinpad(display, title, mask_str, &keypad, None, None)
}

pub fn draw_status_screen<D>(
    display: &mut D,
    card_present: bool,
    usb_configured: bool,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    display.clear(COLOR_BG)?;

    let title_style = MonoTextStyle::new(&FONT_10X20, COLOR_TEXT);
    let status_style = MonoTextStyle::new(&FONT_10X20, COLOR_ACCENT);
    let small_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY);

    Text::new("CCID Reader", Point::new(150, 60), title_style).draw(display)?;
    Text::new("Idle", Point::new(220, 120), status_style).draw(display)?;

    let card_text = if card_present {
        "Card: present"
    } else {
        "Card: absent"
    };
    let usb_text = if usb_configured {
        "USB: configured"
    } else {
        "USB: waiting"
    };

    Text::new(card_text, Point::new(130, 220), title_style).draw(display)?;
    Text::new(usb_text, Point::new(130, 260), title_style).draw(display)?;

    // Version info at bottom
    Text::new("v0.1.0", Point::new(10, 750), small_style).draw(display)?;
    Text::new("git:78bb26e", Point::new(350, 750), small_style).draw(display)?;
    Ok(())
}

/// Touch event handler for pinpad
#[derive(Debug, Clone, Copy)]
pub struct TouchHandler {
    /// Current pressed button
    pressed: Option<ButtonId>,
    /// Touch was active last poll
    was_pressed: bool,
}

impl TouchHandler {
    /// Create a new touch handler
    pub fn new() -> Self {
        Self {
            pressed: None,
            was_pressed: false,
        }
    }

    /// Process a touch event
    ///
    /// Returns Some(ButtonId) when a button press is complete (touch release)
    pub fn process(&mut self, keypad: &Keypad, touch: Option<Point>) -> Option<ButtonId> {
        match touch {
            Some(point) => {
                let hit = keypad.hit_test(point);
                self.pressed = Some(hit);
                self.was_pressed = true;
                None // Don't return until touch release
            }
            None => {
                // Touch released
                let result = if self.was_pressed { self.pressed } else { None };
                self.pressed = None;
                self.was_pressed = false;
                result
            }
        }
    }

    /// Get currently pressed button (for highlighting)
    pub fn pressed(&self) -> Option<ButtonId> {
        self.pressed
    }
}

impl Default for TouchHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypad_creation() {
        let keypad = Keypad::new();
        assert_eq!(keypad.buttons().len(), 13);
    }

    #[test]
    fn test_button_hit_center() {
        let keypad = Keypad::new();

        // Hit button 5 (center of keypad)
        let x = KEYPAD_START_X + BUTTON_SIZE as i32 + BUTTON_SPACING as i32 / 2 + 50;
        let y = KEYPAD_START_Y + BUTTON_SIZE as i32 + BUTTON_SPACING as i32 / 2 + 50;

        let result = keypad.hit_test(Point::new(x, y));
        assert_eq!(result, ButtonId::Digit(5));
    }

    #[test]
    fn test_button_hit_ok() {
        let keypad = Keypad::new();

        // OK button is in bottom right
        let ok_x = KEYPAD_START_X + 2 * (BUTTON_SIZE as i32 + BUTTON_SPACING as i32) + 50;
        let ok_y = KEYPAD_START_Y + 3 * (BUTTON_SIZE as i32 + BUTTON_SPACING as i32) + 50;

        let result = keypad.hit_test(Point::new(ok_x, ok_y));
        assert_eq!(result, ButtonId::Ok);
    }

    #[test]
    fn test_button_hit_cancel() {
        let keypad = Keypad::new();

        // Cancel button is in bottom left
        let cancel_x = KEYPAD_START_X + 50;
        let cancel_y = KEYPAD_START_Y + 3 * (BUTTON_SIZE as i32 + BUTTON_SPACING as i32) + 50;

        let result = keypad.hit_test(Point::new(cancel_x, cancel_y));
        assert_eq!(result, ButtonId::Cancel);
    }

    #[test]
    fn test_no_button_hit() {
        let keypad = Keypad::new();

        // Point outside keypad (top left corner)
        let result = keypad.hit_test(Point::new(10, 10));
        assert_eq!(result, ButtonId::None);
    }

    #[test]
    fn test_button_digit_0() {
        let keypad = Keypad::new();

        // 0 button is in bottom center
        let zero_x = KEYPAD_START_X + BUTTON_SIZE as i32 + BUTTON_SPACING as i32 + 50;
        let zero_y = KEYPAD_START_Y + 3 * (BUTTON_SIZE as i32 + BUTTON_SPACING as i32) + 50;

        let result = keypad.hit_test(Point::new(zero_x, zero_y));
        assert_eq!(result, ButtonId::Digit(0));
    }

    #[test]
    fn test_get_button() {
        let keypad = Keypad::new();

        let ok_button = keypad.get_button(ButtonId::Ok);
        assert!(ok_button.is_some());
        assert_eq!(ok_button.unwrap().label, "OK");

        let cancel_button = keypad.get_button(ButtonId::Cancel);
        assert!(cancel_button.is_some());
        assert_eq!(cancel_button.unwrap().label, "X");
    }

    #[test]
    fn test_touch_handler_press_release() {
        let mut handler = TouchHandler::new();
        let keypad = Keypad::new();

        // Initially no button pressed
        assert_eq!(handler.pressed(), None);

        // Touch on button 5
        let button5_point = Point::new(
            KEYPAD_START_X + BUTTON_SIZE as i32 + BUTTON_SPACING as i32 / 2 + 50,
            KEYPAD_START_Y + BUTTON_SIZE as i32 + BUTTON_SPACING as i32 / 2 + 50,
        );

        // While touching, no result yet
        let result = handler.process(&keypad, Some(button5_point));
        assert_eq!(result, None);
        assert_eq!(handler.pressed(), Some(ButtonId::Digit(5)));

        // On release, return the button
        let result = handler.process(&keypad, None);
        assert_eq!(result, Some(ButtonId::Digit(5)));
        assert_eq!(handler.pressed(), None);
    }

    #[test]
    fn test_touch_handler_no_press() {
        let mut handler = TouchHandler::new();
        let keypad = Keypad::new();

        // No touch, no result
        let result = handler.process(&keypad, None);
        assert_eq!(result, None);
    }

    #[test]
    fn test_touch_handler_outside_button() {
        let mut handler = TouchHandler::new();
        let keypad = Keypad::new();

        // Touch outside any button
        let result = handler.process(&keypad, Some(Point::new(10, 10)));
        assert_eq!(result, None);
        assert_eq!(handler.pressed(), Some(ButtonId::None));

        // Release returns None
        let result = handler.process(&keypad, None);
        assert_eq!(result, Some(ButtonId::None));
    }

    #[test]
    fn test_button_contains() {
        let button = Button::new(ButtonId::Digit(1), 50, 200, "1", COLOR_BUTTON);

        // Inside button
        assert!(button.contains(Point::new(100, 250)));

        // Outside button
        assert!(!button.contains(Point::new(10, 10)));
        assert!(!button.contains(Point::new(200, 250)));
    }
}
