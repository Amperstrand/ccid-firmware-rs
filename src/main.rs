//! STM32F469 CCID Smartcard Reader Firmware
//!
//! This firmware implements a USB CCID class smartcard reader with:
//! - USART2 in smartcard mode for ISO 7816-3 communication
//! - USB OTG FS for CCID protocol communication with host
//! - Touchscreen PIN entry (when display feature enabled)
//!
//! Pin assignments:
//! - PA2: Smartcard IO (USART2_TX, AF7, open-drain)
//! - PA4: Smartcard CLK (USART2_CK, AF7, push-pull)
//! - PG10: Smartcard RST (GPIO output, active LOW)
//! - PC2: Smartcard PRES (GPIO input, HIGH = card present)
//! - PC5: Smartcard PWR (GPIO output, LOW = power ON)
//! - PA11: USB DM
//! - PA12: USB DP
//!
//! Display/Touch (when display feature enabled):
//! - PH7: LCD reset
//! - PB8/PB9: Touch I2C (I2C1)
//! - PC1: Touch interrupt
//! - SDRAM: Framebuffer via FMC

// Entire file is ARM-only firmware code
// For x86_64, cargo test runs against lib.rs instead
#![cfg_attr(all(target_arch = "arm", target_os = "none"), no_std)]
#![cfg_attr(all(target_arch = "arm", target_os = "none"), no_main)]
#![cfg_attr(all(target_arch = "arm", target_os = "none"), allow(unused_mut))]
#![cfg_attr(all(target_arch = "arm", target_os = "none"), allow(unused_variables))]
#![cfg_attr(all(target_arch = "arm", target_os = "none"), allow(static_mut_refs))]

#[cfg(all(target_arch = "arm", target_os = "none"))]
use defmt_rtt as _;
#[cfg(all(target_arch = "arm", target_os = "none"))]
use panic_probe as _;

#[cfg(all(target_arch = "arm", target_os = "none"))]
mod app_enum;
#[cfg(all(target_arch = "arm", target_os = "none"))]
mod device_profile;
#[cfg(all(target_arch = "arm", target_os = "none"))]
mod pinpad;
#[cfg(all(target_arch = "arm", target_os = "none"))]
mod pps_fsm;
#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
mod smartcard;
#[cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
mod smartcard_bitbang;
#[cfg(all(target_arch = "arm", target_os = "none"))]
mod t1_engine;
#[cfg(all(target_arch = "arm", target_os = "none"))]
mod usb_identity;

#[cfg(all(target_arch = "arm", target_os = "none"))]
use cortex_m_rt::entry;

#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
use stm32f4xx_hal::gpio::{
    gpioa::{PA2, PA4},
    gpioc::{PC2, PC5},
    gpiog::PG10,
    Alternate, Input, OpenDrain, Output, PushPull,
};
#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
use stm32f4xx_hal::otg_fs::{UsbBus, USB};
#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
use stm32f4xx_hal::pac;
#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
use stm32f4xx_hal::prelude::*;
#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
use stm32f4xx_hal::rcc::Config;

#[cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
use stm32f7xx_hal::gpio::{
    gpiof::{PF6, PF7},
    gpioi::{PI0, PI2},
    OpenDrain, Output, PushPull,
};
#[cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
use stm32f7xx_hal::otg_fs::{UsbBus, USB};
#[cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
use stm32f7xx_hal::pac;
#[cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
use stm32f7xx_hal::prelude::*;
#[cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
use stm32f7xx_hal::rcc::{HSEClock, HSEClockMode};
#[cfg(all(target_arch = "arm", target_os = "none"))]
use usb_device::endpoint::In;
#[cfg(all(target_arch = "arm", target_os = "none"))]
use usb_device::prelude::*;

#[cfg(feature = "display")]
use crate::pinpad::ui::{
    ButtonId, Keypad, TouchHandler, BUTTON_SIZE, COLOR_ACCENT, COLOR_BG, COLOR_TEXT,
};
#[cfg(feature = "display")]
use crate::pinpad::{PinEntryContext, PinModifyContext, PinModifyStep};
#[cfg(feature = "display")]
use board::hal::ltdc::{Layer, PixelFormat};
#[cfg(feature = "display")]
use board::lcd;
#[cfg(feature = "display")]
use board::sdram::{alt, sdram_pins, Sdram};
#[cfg(feature = "display")]
use board::touch;
#[cfg(feature = "display")]
use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
    text::Text,
    Drawable,
};
#[cfg(feature = "display")]
use stm32f469i_disc as board;

#[cfg(all(target_arch = "arm", target_os = "none"))]
mod ccid;

#[cfg(feature = "display")]
use app_enum::AppEnumerationState;
#[cfg(all(target_arch = "arm", target_os = "none"))]
use ccid::{CcidClass, SmartcardDriver as CcidSmartcardDriver};
#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
use smartcard::{SmartcardError, SmartcardUart};
#[cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
use smartcard_bitbang::{SmartcardBitbang, SmartcardError as BitbangError};
#[cfg(all(target_arch = "arm", target_os = "none"))]
use usb_identity::{
    USB_MANUFACTURER, USB_PRODUCT, USB_PRODUCT_ID, USB_SERIAL_NUMBER, USB_VENDOR_ID,
};

/// USB endpoint memory buffer (required by USB OTG driver)
#[cfg(all(target_arch = "arm", target_os = "none"))]
static mut USB_EP_MEMORY: [u32; 1024] = [0; 1024];

/// Tick counter for timeout handling (milliseconds since boot)
#[cfg(feature = "display")]
static mut TICK_COUNT: u32 = 0;

/// Get current tick count in milliseconds
#[cfg(feature = "display")]
fn get_tick_ms() -> u32 {
    unsafe { TICK_COUNT }
}

/// SysTick exception handler - increments tick counter every 1ms
#[cfg(feature = "display")]
#[cortex_m_rt::exception]
fn SysTick() {
    unsafe {
        TICK_COUNT = TICK_COUNT.wrapping_add(1);
    }
}

/// Application mode state machine
#[cfg(feature = "display")]
enum AppMode {
    /// Normal CCID operation - USB polling, idle screen
    Normal,
    /// PIN entry active - poll USB + touch, render keypad
    PinEntry {
        context: PinEntryContext,
        keypad: Keypad,
        touch_handler: TouchHandler,
        seq: u8,
    },
    /// PIN modification active - multi-step: old PIN → new PIN → confirm (CCID Rev 1.1 §6.1.12)
    PinModify {
        context: PinModifyContext,
        keypad: Keypad,
        touch_handler: TouchHandler,
        seq: u8,
    },
}

/// Wrapper to adapt SmartcardUart to the ccid::SmartcardDriver trait
#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
struct SmartcardWrapper {
    uart: SmartcardUart,
}

#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
impl SmartcardWrapper {
    fn new(uart: SmartcardUart) -> Self {
        Self { uart }
    }
}

#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
impl CcidSmartcardDriver for SmartcardWrapper {
    type Error = SmartcardError;

    fn power_on(&mut self) -> core::result::Result<&[u8], Self::Error> {
        let atr = self.uart.power_on()?;
        Ok(&atr.raw[..atr.len])
    }

    fn power_off(&mut self) {
        self.uart.power_off()
    }

    fn is_card_present(&self) -> bool {
        self.uart.is_card_present()
    }

    fn transmit_apdu(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error> {
        self.uart.transmit_apdu(command, response)
    }

    fn transmit_raw(
        &mut self,
        data: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error> {
        self.uart.transmit_raw(data, response)
    }

    fn set_protocol(&mut self, protocol: u8) {
        self.uart.set_protocol(protocol)
    }

    fn set_clock(&mut self, enable: bool) {
        self.uart.set_clock(enable)
    }

    fn set_clock_and_rate(
        &mut self,
        clock_hz: u32,
        rate_bps: u32,
    ) -> core::result::Result<(u32, u32), Self::Error> {
        self.uart.set_clock_and_rate(clock_hz, rate_bps)
    }
}

/// Wrapper to adapt SmartcardBitbang to the ccid::SmartcardDriver trait
#[cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
struct SmartcardWrapper {
    bitbang: SmartcardBitbang,
}

#[cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
impl SmartcardWrapper {
    fn new(bitbang: SmartcardBitbang) -> Self {
        Self { bitbang }
    }
}

#[cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
impl CcidSmartcardDriver for SmartcardWrapper {
    type Error = BitbangError;

    fn power_on(&mut self) -> core::result::Result<&[u8], Self::Error> {
        self.bitbang.power_on()
    }

    fn power_off(&mut self) {
        self.bitbang.power_off()
    }

    fn is_card_present(&self) -> bool {
        self.bitbang.is_card_present()
    }

    fn transmit_apdu(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error> {
        self.bitbang.transmit_apdu(command, response)
    }

    fn transmit_raw(
        &mut self,
        data: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error> {
        self.bitbang.transmit_raw(data, response)
    }

    fn set_protocol(&mut self, protocol: u8) {
        self.bitbang.set_protocol(protocol)
    }

    fn set_clock(&mut self, enable: bool) {
        self.bitbang.set_clock(enable)
    }

    fn set_clock_and_rate(
        &mut self,
        clock_hz: u32,
        rate_bps: u32,
    ) -> core::result::Result<(u32, u32), Self::Error> {
        self.bitbang.set_clock_and_rate(clock_hz, rate_bps)
    }
}

/// Framebuffer draw target for embedded-graphics
#[cfg(feature = "display")]
struct FrameBufferDrawTarget {
    framebuffer: &'static mut [u16],
    width: u32,
    height: u32,
}

#[cfg(feature = "display")]
impl FrameBufferDrawTarget {
    fn new(framebuffer: &'static mut [u16]) -> Self {
        Self {
            framebuffer,
            width: lcd::WIDTH as u32,
            height: lcd::HEIGHT as u32,
        }
    }

    fn clear(&mut self, color: Rgb565) {
        let color_raw: u16 = color.into_storage();
        for pixel in self.framebuffer.iter_mut() {
            *pixel = color_raw;
        }
    }
}

#[cfg(feature = "display")]
impl Dimensions for FrameBufferDrawTarget {
    fn bounding_box(&self) -> Rectangle {
        Rectangle::new(Point::zero(), Size::new(self.width, self.height))
    }
}
#[cfg(feature = "display")]
impl DrawTarget for FrameBufferDrawTarget {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if point.x >= 0 && point.y >= 0 {
                let x = point.x as u32;
                let y = point.y as u32;
                if x < self.width && y < self.height {
                    let idx = (y * self.width + x) as usize;
                    if idx < self.framebuffer.len() {
                        self.framebuffer[idx] = color.into_storage();
                    }
                }
            }
        }
        Ok(())
    }
}

/// Draw the PIN entry keypad screen
#[cfg(feature = "display")]
fn draw_pin_screen(
    display: &mut FrameBufferDrawTarget,
    context: &PinEntryContext,
    keypad: &Keypad,
    pressed_button: Option<ButtonId>,
) {
    // Clear screen
    display.clear(COLOR_BG);

    // Draw title
    let title_style = MonoTextStyle::new(&FONT_10X20, COLOR_TEXT);
    let title = "Enter PIN";
    let title_x = (display.width as i32 / 2).saturating_sub((title.len() as i32 * 10) / 2);
    let _ = Text::new(title, Point::new(title_x, 50), title_style).draw(display);

    // Draw PIN mask (centered)
    let pin_len = context.buffer.len();
    let mut mask_buf = [b'*'; 16];
    let mask_str = core::str::from_utf8(&mask_buf[..pin_len.min(16)]).unwrap_or("****");
    let pin_style = MonoTextStyle::new(&FONT_10X20, COLOR_ACCENT);
    let pin_x = (display.width as i32 / 2).saturating_sub((pin_len.min(16) as i32 * 10) / 2);
    let _ = Text::new(mask_str, Point::new(pin_x, 130), pin_style).draw(display);

    // Draw keypad buttons
    for button in keypad.buttons() {
        let color = if pressed_button == Some(button.id) {
            Rgb565::CSS_SLATE_GRAY
        } else {
            button.color
        };

        // Draw button background
        let style = PrimitiveStyleBuilder::new()
            .fill_color(color)
            .stroke_color(COLOR_TEXT)
            .stroke_width(2)
            .build();

        let _ = button.bounds.into_styled(style).draw(display);

        // Draw button label (centered)
        let label_style = MonoTextStyle::new(&FONT_10X20, COLOR_TEXT);
        let label_x =
            button.bounds.top_left.x + (BUTTON_SIZE as i32 / 2) - (button.label.len() as i32 * 5);
        let label_y = button.bounds.top_left.y + (BUTTON_SIZE as i32 / 2) + 10;
        let _ = Text::new(button.label, Point::new(label_x, label_y), label_style).draw(display);
    }
}

/// Draw the PIN modification keypad screen with step-aware prompts (CCID Rev 1.1 §6.1.12)
#[cfg(feature = "display")]
fn draw_pin_modify_screen(
    display: &mut FrameBufferDrawTarget,
    context: &PinModifyContext,
    keypad: &Keypad,
    pressed_button: Option<ButtonId>,
) {
    display.clear(COLOR_BG);

    let title_style = MonoTextStyle::new(&FONT_10X20, COLOR_TEXT);

    let (title, subtitle) = match context.step {
        PinModifyStep::OldPin => ("Change PIN", "Enter current PIN"),
        PinModifyStep::NewPin => ("Change PIN", "Enter new PIN"),
        PinModifyStep::ConfirmPin => ("Change PIN", "Confirm new PIN"),
        PinModifyStep::Completed => ("Change PIN", "Done!"),
        PinModifyStep::Cancelled => ("Change PIN", "Cancelled"),
        PinModifyStep::Timeout => ("Change PIN", "Timeout"),
        PinModifyStep::Mismatch => ("Change PIN", "PINs don't match"),
        PinModifyStep::InvalidLength => ("Change PIN", "Invalid length"),
        PinModifyStep::Idle => ("Change PIN", ""),
    };

    let title_x = (display.width as i32 / 2).saturating_sub((title.len() as i32 * 10) / 2);
    let _ = Text::new(title, Point::new(title_x, 30), title_style).draw(display);

    let subtitle_style = MonoTextStyle::new(&FONT_10X20, COLOR_ACCENT);
    let subtitle_x = (display.width as i32 / 2).saturating_sub((subtitle.len() as i32 * 10) / 2);
    let _ = Text::new(subtitle, Point::new(subtitle_x, 60), subtitle_style).draw(display);

    let pin_len = context.current_buffer_len();
    let mut mask_buf = [b'*'; 16];
    let mask_str = core::str::from_utf8(&mask_buf[..pin_len.min(16)]).unwrap_or("****");
    let pin_style = MonoTextStyle::new(&FONT_10X20, COLOR_ACCENT);
    let pin_x = (display.width as i32 / 2).saturating_sub((pin_len.min(16) as i32 * 10) / 2);
    let _ = Text::new(mask_str, Point::new(pin_x, 130), pin_style).draw(display);

    for button in keypad.buttons() {
        let color = if pressed_button == Some(button.id) {
            Rgb565::CSS_SLATE_GRAY
        } else {
            button.color
        };

        let style = PrimitiveStyleBuilder::new()
            .fill_color(color)
            .stroke_color(COLOR_TEXT)
            .stroke_width(2)
            .build();

        let _ = button.bounds.into_styled(style).draw(display);

        let label_style = MonoTextStyle::new(&FONT_10X20, COLOR_TEXT);
        let label_x =
            button.bounds.top_left.x + (BUTTON_SIZE as i32 / 2) - (button.label.len() as i32 * 5);
        let label_y = button.bounds.top_left.y + (BUTTON_SIZE as i32 / 2) + 10;
        let _ = Text::new(button.label, Point::new(label_x, label_y), label_style).draw(display);
    }
}

/// Draw idle/status screen
#[cfg(feature = "display")]
fn draw_idle_screen(
    display: &mut FrameBufferDrawTarget,
    card_present: bool,
    detected_apps: &[&str],
) {
    display.clear(COLOR_BG);

    let title_style = MonoTextStyle::new(&FONT_10X20, COLOR_TEXT);
    let status_style = MonoTextStyle::new(&FONT_10X20, COLOR_ACCENT);

    let _ = Text::new("CCID Reader", Point::new(150, 60), title_style).draw(display);
    let _ = Text::new("Idle", Point::new(220, 120), status_style).draw(display);

    let card_text = if card_present {
        "Card: present"
    } else {
        "Card: absent"
    };
    let _ = Text::new(card_text, Point::new(130, 220), title_style).draw(display);

    // Display detected apps below card status
    let small_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY);
    let mut y = 260;
    for app_name in detected_apps {
        let _ = Text::new(app_name, Point::new(150, y), small_style).draw(display);
        y += 40;
    }

    // USB status below apps (or at 260 if no apps)
    let usb_y = if detected_apps.is_empty() { 260 } else { y };
    let _ = Text::new("USB: ready", Point::new(130, usb_y), title_style).draw(display);

    let version = option_env!("GIT_VERSION").unwrap_or("unknown");
    let _ = Text::new(version, Point::new(10, 750), small_style).draw(display);
}

#[cfg(all(target_arch = "arm", target_os = "none"))]
#[entry]
fn main() -> ! {
    defmt::info!("CCID Reader starting...");

    // =========================================================================
    // Step 1: Take peripherals
    // =========================================================================
    let dp = pac::Peripherals::take().unwrap();

    // =========================================================================
    // Step 2: Configure clocks
    // =========================================================================
    defmt::info!("Configuring clocks...");

    #[cfg(all(feature = "stm32f469", feature = "display"))]
    let (mut rcc, mut delay) = {
        use cortex_m::peripheral::Peripherals;
        let cp = Peripherals::take().unwrap();
        let rcc = dp.RCC.freeze(
            Config::hse(8.MHz())
                .pclk1(42.MHz())
                .pclk2(84.MHz())
                .sysclk(168.MHz())
                .require_pll48clk(),
        );
        let delay = cp.SYST.delay(&rcc.clocks);
        (rcc, delay)
    };

    #[cfg(all(feature = "stm32f469", not(feature = "display")))]
    let mut rcc = dp.RCC.freeze(
        Config::hse(8.MHz())
            .sysclk(168.MHz())
            .pclk1(42.MHz())
            .pclk2(84.MHz())
            .require_pll48clk(),
    );

    #[cfg(feature = "stm32f746")]
    let clocks = {
        let rcc = dp.RCC.constrain();
        rcc.cfgr
            .hse(HSEClock::new(25.MHz(), HSEClockMode::Oscillator))
            .sysclk(216.MHz())
            .pclk1(54.MHz())
            .pclk2(108.MHz())
            .use_pll48clk(stm32f7xx_hal::rcc::PLL48CLK::Pllq)
            .freeze()
    };
    #[cfg(feature = "stm32f746")]
    defmt::info!("PLL48CLK valid: {}", clocks.is_pll48clk_valid());

    defmt::info!("Clocks OK");

    // =========================================================================
    // Step 3: Configure GPIO for smartcard and USB
    // =========================================================================

    #[cfg(feature = "stm32f469")]
    let smartcard_driver = {
        let mut gpioa = dp.GPIOA.split(&mut rcc);
        let mut gpioc = dp.GPIOC.split(&mut rcc);
        let mut gpiog = dp.GPIOG.split(&mut rcc);

        let io_pin: PA2<Alternate<7, OpenDrain>> = gpioa
            .pa2
            .into_alternate_open_drain::<7>()
            .internal_pull_up(true)
            .speed(stm32f4xx_hal::gpio::Speed::High);

        let clk_pin: PA4<Alternate<7, PushPull>> = gpioa
            .pa4
            .into_alternate::<7>()
            .speed(stm32f4xx_hal::gpio::Speed::High);

        let rst_pin: PG10<Output<PushPull>> = gpiog
            .pg10
            .into_push_pull_output_in_state(stm32f4xx_hal::gpio::PinState::High);

        let pres_pin: PC2<Input> = gpioc.pc2.into_input();
        let pwr_pin: PC5<Output<PushPull>> = gpioc
            .pc5
            .into_push_pull_output_in_state(stm32f4xx_hal::gpio::PinState::High);

        defmt::info!("Smartcard GPIO OK");

        let usb_dm = gpioa.pa11.into_alternate::<10>();
        let usb_dp = gpioa.pa12.into_alternate::<10>();

        let uart = SmartcardUart::new(
            dp.USART2,
            io_pin,
            clk_pin,
            rst_pin,
            pres_pin,
            pwr_pin,
            &rcc.clocks,
        );
        defmt::info!("Smartcard UART OK");

        (SmartcardWrapper::new(uart), usb_dm, usb_dp)
    };

    #[cfg(feature = "stm32f746")]
    let smartcard_driver = {
        let mut gpiof = dp.GPIOF.split();
        let mut gpioi = dp.GPIOI.split();

        // PI0: Smartcard IO (open-drain, pulled up)
        let io_pin: PI0<Output<OpenDrain>> = gpioi
            .pi0
            .into_open_drain_output()
            .internal_pull_up(true)
            .set_speed(stm32f7xx_hal::gpio::Speed::High);

        // PF6: Smartcard CLK (GPIO toggle for bitbang)
        let clk_pin: PF6<Output<PushPull>> = gpiof
            .pf6
            .into_push_pull_output_in_state(stm32f7xx_hal::gpio::PinState::Low)
            .set_speed(stm32f7xx_hal::gpio::Speed::VeryHigh);

        // PI2: Smartcard RST (output, active LOW, initial HIGH)
        let rst_pin: PI2<Output<PushPull>> = gpioi
            .pi2
            .into_push_pull_output_in_state(stm32f7xx_hal::gpio::PinState::High);

        // PF10: Smartcard PRES (input, HIGH = card present)
        let pres_pin = gpiof.pf10.into_floating_input();

        // PF7: Smartcard PWR (output, LOW = power ON, initial HIGH = OFF)
        let pwr_pin: PF7<Output<PushPull>> = gpiof
            .pf7
            .into_push_pull_output_in_state(stm32f7xx_hal::gpio::PinState::High);

        defmt::info!("Smartcard GPIO OK");

        let mut gpioa = dp.GPIOA.split();
        let usb_dm = gpioa.pa11.into_alternate::<10>();
        let usb_dp = gpioa.pa12.into_alternate::<10>();

        let sysclk_hz = clocks.sysclk().raw();
        let bitbang = SmartcardBitbang::new(io_pin, clk_pin, rst_pin, pres_pin, pwr_pin, sysclk_hz);
        defmt::info!("Smartcard bitbang OK");

        (SmartcardWrapper::new(bitbang), usb_dm, usb_dp)
    };

    let (smartcard_wrapper, usb_dm, usb_dp) = smartcard_driver;

    // =========================================================================
    // Step 5: USB OTG FS
    // =========================================================================
    #[cfg(feature = "stm32f469")]
    let usb_otg = USB::new(
        (dp.OTG_FS_GLOBAL, dp.OTG_FS_DEVICE, dp.OTG_FS_PWRCLK),
        (usb_dm, usb_dp),
        &rcc.clocks,
    );
    #[cfg(feature = "stm32f746")]
    let usb_otg = USB::new(
        dp.OTG_FS_GLOBAL,
        dp.OTG_FS_DEVICE,
        dp.OTG_FS_PWRCLK,
        (usb_dm, usb_dp),
        &clocks,
    );
    let usb_bus = UsbBus::new(usb_otg, unsafe { &mut USB_EP_MEMORY });
    defmt::info!("USB bus OK");

    // =========================================================================
    // Step 6: CCID class
    // =========================================================================
    let ep_int = usb_bus.interrupt::<In>(8, 10);
    let mut ccid_class = CcidClass::new(&usb_bus, smartcard_wrapper, ep_int);
    defmt::info!("CCID class OK");

    let mut usb_device = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(USB_VENDOR_ID, USB_PRODUCT_ID))
        .strings(&[StringDescriptors::default()
            .manufacturer(USB_MANUFACTURER)
            .product(USB_PRODUCT)
            .serial_number(USB_SERIAL_NUMBER)])
        .unwrap()
        .device_class(0x00)
        .build();

    defmt::info!("USB device OK");

    // =========================================================================
    // Step 7: Display/Touch initialization (feature-gated)
    // =========================================================================
    #[cfg(feature = "display")]
    let mut display_state = {
        defmt::info!("Initializing display/touch...");

        // GPIO splits for display
        // Note: gpioc and gpiog already split above for smartcard pins
        // We use the remaining pins from those splits
        let gpiob = dp.GPIOB.split(&mut rcc);
        let gpiod = dp.GPIOD.split(&mut rcc);
        let gpioe = dp.GPIOE.split(&mut rcc);
        let gpiof = dp.GPIOF.split(&mut rcc);
        let gpioh = dp.GPIOH.split(&mut rcc);
        let gpioi = dp.GPIOI.split(&mut rcc);

        // LCD reset
        let mut lcd_reset = gpioh.ph7.into_push_pull_output();
        lcd_reset.set_low();
        delay.delay_ms(20u32);
        lcd_reset.set_high();
        delay.delay_ms(10u32);

        // Touch I2C - MUST extract ts_int BEFORE sdram_pins!
        defmt::info!("Initializing touch I2C...");
        let mut i2c = touch::init_i2c(dp.I2C1, gpiob.pb8, gpiob.pb9, &mut rcc);
        let ts_int = gpioc.pc1.into_pull_down_input();

        // SDRAM for framebuffer
        defmt::info!("Initializing SDRAM...");
        let sdram = Sdram::new(
            dp.FMC,
            sdram_pins! {gpioc, gpiod, gpioe, gpiof, gpiog, gpioh, gpioi},
            &rcc.clocks,
            &mut delay,
        );

        // Framebuffer pointer
        let fb_ptr: *mut u16 = sdram.mem as *mut u16;

        // Display init
        defmt::info!("Initializing display...");
        let (mut display_ctrl, _) = lcd::init_display_full(
            dp.DSI,
            dp.LTDC,
            dp.DMA2D,
            &mut rcc,
            &mut delay,
            lcd::BoardHint::Unknown,
            PixelFormat::RGB565,
        );

        // Create static framebuffer slice for config_layer
        let framebuffer: &'static mut [u16] =
            unsafe { core::slice::from_raw_parts_mut(fb_ptr, lcd::FB_SIZE) };
        display_ctrl.config_layer(Layer::L1, framebuffer, PixelFormat::RGB565);
        display_ctrl.enable_layer(Layer::L1);
        display_ctrl.reload();

        // Touch controller
        defmt::info!("Initializing touch controller...");
        let touch_ctrl = touch::init_ft6x06(&i2c, ts_int);
        if touch_ctrl.is_some() {
            defmt::info!("FT6X06 touch controller initialized");
        } else {
            defmt::warn!("FT6X06 touch controller not detected");
        }

        // Create draw target from the same SDRAM memory (new slice from raw pointer)
        // This is safe because SDRAM is static memory
        let draw_target_framebuffer: &'static mut [u16] =
            unsafe { core::slice::from_raw_parts_mut(fb_ptr, lcd::FB_SIZE) };
        let draw_target = FrameBufferDrawTarget::new(draw_target_framebuffer);

        Some((draw_target, touch_ctrl, i2c))
    };

    #[cfg(not(feature = "display"))]
    let display_state: Option<()> = None;

    defmt::info!("Entering main loop");

    // =========================================================================
    // Step 8: Main loop
    // =========================================================================
    #[cfg(feature = "display")]
    let mut mode = AppMode::Normal;

    #[cfg(feature = "display")]
    let mut last_card_present = false;

    #[cfg(feature = "display")]
    let mut app_enum_state = AppEnumerationState::new();

    loop {
        // Always poll USB - required for both normal CCID and PIN entry modes
        usb_device.poll(&mut [&mut ccid_class]);

        #[cfg(feature = "display")]
        {
            match &mut mode {
                AppMode::Normal => {
                    if ccid_class.is_pin_entry_active() {
                        if let Some((seq, params)) = ccid_class.take_secure_params() {
                            defmt::info!("Entering PIN mode, seq={}", seq);
                            let mut context = PinEntryContext::new(params);
                            context.start(get_tick_ms());
                            let keypad = Keypad::new();
                            let touch_handler = TouchHandler::new();
                            mode = AppMode::PinEntry {
                                context,
                                keypad,
                                touch_handler,
                                seq,
                            };
                        }
                    } else if ccid_class.is_pin_modify_active() {
                        // Check for PIN modify (CCID Rev 1.1 §6.1.12)
                        if let Some((seq, params)) = ccid_class.take_secure_modify_params() {
                            defmt::info!("Entering PIN modify mode, seq={}", seq);
                            let mut context = PinModifyContext::new(params);
                            context.start(get_tick_ms());
                            let keypad = Keypad::new();
                            let touch_handler = TouchHandler::new();
                            mode = AppMode::PinModify {
                                context,
                                keypad,
                                touch_handler,
                                seq,
                            };
                        }
                    } else if let Some((ref mut draw_target, _, _)) = display_state {
                        let card_present = ccid_class.is_card_present();
                        let card_active = ccid_class.is_card_active();
                        if card_present != last_card_present {
                            if !card_present {
                                app_enum_state.reset();
                            }
                            let mut detected: [&str; 5] = ["", "", "", "", ""];
                            let mut count = 0;
                            for name in app_enum_state.detected_names() {
                                if count < 5 {
                                    detected[count] = name;
                                    count += 1;
                                }
                            }
                            draw_idle_screen(draw_target, card_present, &detected[..count]);
                            last_card_present = card_present;
                        } else if card_active && !app_enum_state.is_enumerated() {
                            app_enum_state
                                .enumerate_if_needed(ccid_class.driver_mut(), card_active);
                            let mut detected: [&str; 5] = ["", "", "", "", ""];
                            let mut count = 0;
                            for name in app_enum_state.detected_names() {
                                if count < 5 {
                                    detected[count] = name;
                                    count += 1;
                                }
                            }
                            draw_idle_screen(draw_target, card_present, &detected[..count]);
                        }
                    }
                }
                AppMode::PinEntry {
                    context,
                    keypad,
                    touch_handler,
                    seq,
                } => {
                    // Poll touch
                    let touch_point =
                        if let Some((_, ref mut touch_ctrl, ref mut i2c)) = display_state {
                            if let Some(ref mut t) = touch_ctrl {
                                if let Ok(num) = t.detect_touch(i2c) {
                                    if num > 0 {
                                        if let Ok(point) = t.get_touch(i2c, 1) {
                                            Some(Point::new(point.x as i32, point.y as i32))
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                    // Process through TouchHandler
                    let button = touch_handler.process(keypad, touch_point);

                    // Handle button press
                    if let Some(btn) = button {
                        match btn {
                            ButtonId::Digit(d) => {
                                context.add_digit(d);
                                defmt::debug!("PIN digit: {}", d);
                            }
                            ButtonId::Backspace => {
                                context.backspace();
                            }
                            ButtonId::Ok => {
                                context.submit();
                                defmt::info!("PIN submitted, len={}", context.buffer.len());
                            }
                            ButtonId::Cancel => {
                                context.cancel();
                                defmt::info!("PIN cancelled");
                            }
                            ButtonId::None => {}
                        }
                    }

                    // Check timeout
                    if context.check_timeout(get_tick_ms(), 1000) {
                        defmt::warn!("PIN entry timeout");
                    }

                    // Redraw screen
                    if let Some((ref mut draw_target, _, _)) = display_state {
                        draw_pin_screen(draw_target, context, keypad, touch_handler.pressed());
                    }

                    // Check if complete
                    if context.is_complete() {
                        if let Some(result) = context.result() {
                            defmt::info!("PIN entry complete: {:?}", result);
                            let params = context.params;
                            let buffer = context.buffer.clone();
                            ccid_class.set_pin_result(*seq, result, buffer, params);
                        }
                        mode = AppMode::Normal;
                        last_card_present = ccid_class.is_card_present();
                        defmt::debug!(
                            "Returned to Normal mode, card_present={}",
                            last_card_present
                        );
                    }
                }
                AppMode::PinModify {
                    context,
                    keypad,
                    touch_handler,
                    seq,
                } => {
                    let touch_point =
                        if let Some((_, ref mut touch_ctrl, ref mut i2c)) = display_state {
                            if let Some(ref mut t) = touch_ctrl {
                                if let Ok(num) = t.detect_touch(i2c) {
                                    if num > 0 {
                                        if let Ok(point) = t.get_touch(i2c, 1) {
                                            Some(Point::new(point.x as i32, point.y as i32))
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                    let button = touch_handler.process(keypad, touch_point);

                    if let Some(btn) = button {
                        match btn {
                            ButtonId::Digit(d) => {
                                context.add_digit(d);
                                defmt::debug!("PIN modify digit: {}", d);
                            }
                            ButtonId::Backspace => {
                                context.backspace();
                            }
                            ButtonId::Ok => {
                                let result = context.submit();
                                defmt::info!("PIN modify step submitted, result={:?}", result);
                            }
                            ButtonId::Cancel => {
                                context.cancel();
                                defmt::info!("PIN modify cancelled");
                            }
                            ButtonId::None => {}
                        }
                    }

                    if context.check_timeout(get_tick_ms(), 1000) {
                        defmt::warn!("PIN modify timeout");
                    }

                    if let Some((ref mut draw_target, _, _)) = display_state {
                        draw_pin_modify_screen(
                            draw_target,
                            context,
                            keypad,
                            touch_handler.pressed(),
                        );
                    }

                    if context.is_complete() {
                        if let Some(result) = context.result() {
                            defmt::info!("PIN modify complete: {:?}", result);
                            ccid_class.set_pin_modify_result(
                                *seq,
                                result,
                                context.old_buffer.clone(),
                                context.new_buffer.clone(),
                                context.params,
                            );
                        }
                        mode = AppMode::Normal;
                        last_card_present = ccid_class.is_card_present();
                        defmt::debug!(
                            "Returned to Normal mode, card_present={}",
                            last_card_present
                        );
                    }
                }
            }

            ccid_class.process_pin_result();
            ccid_class.process_pin_modify_result();
        }

        #[cfg(not(feature = "display"))]
        {
            // No display - nothing else to do
        }
    }
}

// Stub main for host compilation (tests, docs, etc.)
// The firmware binary only runs on ARM targets, but cargo test
// on CI needs a valid main() for the x86_64 host.
#[cfg(not(all(target_arch = "arm", target_os = "none")))]
fn main() {
    // This binary is only meaningful on ARM embedded targets
}
