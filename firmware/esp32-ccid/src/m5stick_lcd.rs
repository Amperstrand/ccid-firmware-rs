//! M5StickC(-Plus) LCD status screen — ported from bolty-rs
//! `apps/bolty-esp32/src/display.rs` (same esp-idf-hal =0.46.2 /
//! esp-idf-svc =0.52.1 stack, bench-proven on this lab's M5Stick
//! hardware), which itself carries wiring derived from M5GFX /
//! M5Unified 0.2.7 (m5-cashu-mint `m5stick-lcd`).
//!
//! Architecture (unchanged from bolty): one `init()` at boot claims
//! I2C1 (AXP192 PMU), SPI2 + panel pins; a static `Mutex<Option<Screen>>`
//! holds the mipidsi display; setters mutate `ScreenState` under the
//! mutex and lazily redraw on the `dirty` flag. CCID serving NEVER
//! depends on the display — every failure path logs and continues.
//!
//! GREAT EXPECTATION (AGENTS.md): every reference-derived byte sequence
//! below (AXP192 register ops, panel geometry) carries a citation and a
//! host unit test that locks it (`axp192_init_sequence_matches_reference`,
//! `panel_geometry_matches_m5gfx`). If a test breaks, the breaker
//! re-verifies against the citations — never "fixes" the test.

use core::fmt::Write as _;

// ---------------------------------------------------------------------------
// Reference-derived constants — GREAT EXPECTATION, locked by tests below.
// ---------------------------------------------------------------------------

/// AXP192 PMU I2C address — M5GFX `axp_i2c_addr = 0x34` (ESP32 section,
/// M5GFX.cpp:288-291), AXP192 datasheet §7.1 (7-bit 0110100).
pub const AXP192_I2C_ADDR: u8 = 0x34;

/// AXP192 register map — names track the datasheet (the pmic.rs
/// precedent: jl1nie/mfsk-core embedded-poc, "register addresses we
/// touch, names track the AXP192 datasheet"). Every literal below was
/// previously an unexplained magic number in the op-table.
pub const REG_POWER_STATUS: u8 = 0x01; // §8.2 — VBUS bit5 = USB powered
pub const REG_CHIP_ID: u8 = 0x03; // §8.3 — reads 0x03 on AXP192 (liveness probe)
pub const REG_DCDC1_VOLTAGE: u8 = 0x10; // §8.19 — bit2 = Grove 5V boost EXTEN
pub const REG_POWER_OUT_CTL: u8 = 0x12; // §8.5 — DCDC1|DCDC3|LDO2|LDO3|EXTEN enables
pub const REG_CHARGE_CTL1: u8 = 0x33; // §8.24 — charge target/rate
pub const REG_PEK_KEY_MODE: u8 = 0x36; // §8.28 — power-on/off timing
pub const REG_ADC_EN_1: u8 = 0x82; // §8.33 — ADC channel enables (battery telemetry)
pub const REG_GPIO0_FUNCTION: u8 = 0x90; // §8.38 — GPIO0 LDO mode
pub const REG_GPIO0_LDO_VOLTAGE: u8 = 0x91; // §8.39 — GPIO0 LDO voltage (RTC rail 3.3V on M5StickC)
pub const REG_LDO2_LDO3_VOLTAGE: u8 = 0x28; // §8.22 — hi-nibble LDO2, lo-nibble LDO3, 1.8V + 0.1V/step
pub const REG_BATT_VOLTAGE_H: u8 = 0x78; // §8.26 — battery mV, high byte
pub const REG_BATT_VOLTAGE_L: u8 = 0x79; // §8.26 — battery mV, low nibble

/// LDO2/LDO3 voltages: 0xCC → both 3.0 V (high nibble = LDO2 in 100 mV
/// steps above 1.8 V: 0xC = 12 → 3.0 V). LDO2 = LCD backlight rail,
/// LDO3 = display rail. Source: bolty display.rs `init_axp192` reg 0x28
/// write; cross-checked M5GFX `Light_M5StickC::setBrightness` (reg 0x28
/// high-nibble voltage, reg 0x12 bit2 enable).
const AXP192_REG28_LDO_VOLTAGE: u8 = 0xCC;

/// Power-output control bits to OR into reg 0x12:
/// 0x4D = DCDC1 (0x01) | LDO2 (0x04) | LDO3 (0x08) | EXTEN (0x40).
/// AXP192 datasheet reg 12h "Power Output Control"; same bits as bolty
/// `init_axp192` (`buf[0] | 0x4D`) and M5Unified Power_Class.
const AXP192_REG12_ENABLE_BITS: u8 = 0x4D;

/// Grove 5 V boost enable: bit 2 of DC-DC1 voltage reg 0x10
/// (bolty `init_axp192`; the Grove port must be powered for the
/// MFRC522 on `board-m5stick` builds — reg 0x10 bit2 = EXTEN of
/// the 5 V boost per AXP192 datasheet §8.19 DC1).
const AXP192_REG10_GROVE_BOOST_BIT: u8 = 1 << 2;

/// ST7789 panel geometry — M5GFX `Panel_M5StickCPlus`
/// (M5GFX.cpp:475-485): 135x240 window at offset (52, 40), colors
/// inverted. The physical panel is 240x320; the StickC-Plus LCD is the
/// centered 135x240 crop.
pub const LCD_H_RES: u16 = 135;
pub const LCD_V_RES: u16 = 240;
pub const DISPLAY_OFFSET_X: u16 = 52;
pub const DISPLAY_OFFSET_Y: u16 = 40;

/// The AXP192 bring-up as pure data — executed on-target over I2C1,
/// locked on-host by `axp192_init_sequence_matches_reference`.
/// `Set` writes the register; `Or` is a read-modify-write OR.
pub enum AxpOp {
    Set {
        reg: u8,
        value: u8,
        why: &'static str,
    },
    Or {
        reg: u8,
        bits: u8,
        why: &'static str,
    },
}

pub const AXP192_INIT_SEQUENCE: &[AxpOp] = &[
    AxpOp::Set {
        reg: REG_LDO2_LDO3_VOLTAGE,
        value: AXP192_REG28_LDO_VOLTAGE,
        why: "LDO2=3.0V (backlight) LDO3=3.0V (display)",
    },
    AxpOp::Or {
        reg: REG_POWER_OUT_CTL,
        bits: AXP192_REG12_ENABLE_BITS,
        why: "enable DCDC1+LDO2+LDO3+EXTEN",
    },
    AxpOp::Set {
        reg: REG_ADC_EN_1,
        value: 0xFF,
        why: "ADC all enabled (battery telemetry)",
    },
    AxpOp::Set {
        reg: REG_CHARGE_CTL1,
        value: 0xC0,
        why: "charge 4.2V/100mA",
    },
    AxpOp::Set {
        reg: REG_PEK_KEY_MODE,
        value: 0x0C,
        why: "poweron/off timing",
    },
    AxpOp::Set {
        reg: REG_GPIO0_LDO_VOLTAGE,
        value: 0xF0,
        why: "RTC 3.3V",
    },
    AxpOp::Set {
        reg: REG_GPIO0_FUNCTION,
        value: 0x02,
        why: "GPIO0 LDO mode",
    },
    AxpOp::Or {
        reg: REG_DCDC1_VOLTAGE,
        bits: AXP192_REG10_GROVE_BOOST_BIT,
        why: "Grove 5V boost enable (MFRC522 rail)",
    },
];

// ---------------------------------------------------------------------------
// Screen state — pure data + pure formatting, host-testable.
// ---------------------------------------------------------------------------

/// Battery register decode (AXP192 datasheet §8.26 reg 78h/79h):
/// raw = reg78[7:0] << 4 | reg79[7:4], 1.1 mV/LSB → (raw * 11) / 10 mV.
pub fn battery_mv_from_regs(reg_batt_h: u8, reg_batt_l: u8) -> u32 {
    let raw = ((reg_batt_h as u32) << 4) | ((reg_batt_l as u32) >> 4);
    (raw * 11) / 10
}

/// Battery percent from millivolts — the bolty piecewise map
/// (3.0 V floor .. 4.2 V ceiling, 12 mV/%).
pub fn battery_pct_from_mv(mv: u32) -> u8 {
    if mv <= 3000 {
        0
    } else if mv >= 4200 {
        100
    } else {
        ((mv - 3000) / 12) as u8
    }
}

/// USB-powered: reg 0x01 bit 5 (ACIN/VBUS present — AXP192 datasheet
/// §8.2 power path status; bit5 = VBUS-VALID).
pub fn usb_powered_from_reg01(reg01: u8) -> bool {
    reg01 & (1 << 5) != 0
}

/// What the status screen shows. FWID/rev come from the build env
/// (`build.rs` FW_GIT_REV / FW_BUILD_TS — the same identity the boot
/// banner and Escape 0x02 use).
pub struct ScreenData {
    pub fwid_line: heapless::String<24>,
    pub variant: &'static str,
    pub card_present: bool,
    pub battery_pct: u8,
    pub usb_power: bool,
    pub uptime_s: u32,
}

impl ScreenData {
    pub fn new(fwid_rev: &str, variant: &'static str) -> Self {
        let mut fwid_line = heapless::String::new();
        let _ = write!(fwid_line, "CCID {fwid_rev}");
        Self {
            fwid_line,
            variant,
            card_present: false,
            battery_pct: 255,
            usb_power: false,
            uptime_s: 0,
        }
    }
}

/// Pure line-builder for the screen — the host-testable contract of
/// what the display renders. 135 px / 6 px glyphs = 22 usable chars.
pub fn status_lines(d: &ScreenData) -> [heapless::String<22>; 5] {
    let mut l0 = heapless::String::new();
    let _ = write!(l0, "{}", d.fwid_line);

    let mut l1 = heapless::String::new();
    let _ = write!(l1, "{}", d.variant);

    let mut l2 = heapless::String::new();
    let _ = write!(
        l2,
        "card: {}",
        if d.card_present { "PRESENT" } else { "---" }
    );

    let mut l3 = heapless::String::new();
    if d.battery_pct == 255 {
        let _ = write!(l3, "batt: ?");
    } else {
        let _ = write!(
            l3,
            "batt: {}%{}",
            d.battery_pct,
            if d.usb_power { "+" } else { "" }
        );
    }

    let mut l4 = heapless::String::new();
    let hours = d.uptime_s / 3600;
    let mins = (d.uptime_s % 3600) / 60;
    if hours > 0 {
        let _ = write!(l4, "up: {hours}h{mins:02}m");
    } else {
        let _ = write!(l4, "up: {mins}m{:02}s", d.uptime_s % 60);
    }

    [l0, l1, l2, l3, l4]
}

// ---------------------------------------------------------------------------
// Target-only driver section — the bolty port proper.
// ---------------------------------------------------------------------------

#[cfg(all(target_arch = "xtensa", feature = "board-m5stick", feature = "lcd"))]
mod imp {
    use super::*;

    use embedded_graphics::{
        mono_font::{ascii::FONT_6X10, MonoTextStyleBuilder},
        pixelcolor::Rgb565,
        prelude::*,
        primitives::{PrimitiveStyleBuilder, Rectangle},
        text::{Baseline, Text},
    };
    use esp_idf_hal::{
        delay::FreeRtos,
        gpio::{
            Gpio12, Gpio13, Gpio15, Gpio18, Gpio21, Gpio22, Gpio23, Gpio27, Gpio5, Output,
            PinDriver,
        },
        i2c::{I2cConfig, I2cDriver, I2C1},
        spi::{
            config::{Config, DriverConfig},
            SpiDeviceDriver, SpiDriver,
        },
        units::FromValueType,
    };
    use log::{info, warn};
    use mipidsi::{models::ST7789, options::ColorInversion, Builder};

    const SPI_BUFFER_SIZE: usize = 512;
    const AXP192_TIMEOUT_MS: u32 = 100;

    type LcdDisplay = mipidsi::Display<
        mipidsi::interface::SpiInterface<
            'static,
            SpiDeviceDriver<'static, SpiDriver<'static>>,
            PinDriver<'static, Output>,
        >,
        ST7789,
        PinDriver<'static, Output>,
    >;

    struct Screen {
        display: LcdDisplay,
        data: ScreenData,
        dirty: bool,
    }

    static mut SPI_BUFFER: [u8; SPI_BUFFER_SIZE] = [0u8; SPI_BUFFER_SIZE];

    static SCREEN: std::sync::Mutex<Option<Screen>> = std::sync::Mutex::new(None);

    static I2C1_DRIVER: std::sync::Mutex<Option<I2cDriver<'static>>> = std::sync::Mutex::new(None);

    fn axp_write(i2c: &mut I2cDriver<'_>, reg: u8, val: u8) -> Result<(), &'static str> {
        i2c.write(super::AXP192_I2C_ADDR, &[reg, val], AXP192_TIMEOUT_MS)
            .map_err(|e| {
                warn!("AXP192 write reg 0x{reg:02X} failed: {e:?}");
                "axp192 i2c write failed"
            })
    }

    fn axp_read(i2c: &mut I2cDriver<'_>, reg: u8) -> Result<u8, &'static str> {
        let mut buf = [0u8; 1];
        i2c.write_read(super::AXP192_I2C_ADDR, &[reg], &mut buf, AXP192_TIMEOUT_MS)
            .map_err(|e| {
                warn!("AXP192 read reg 0x{reg:02X} failed: {e:?}");
                "axp192 i2c read failed"
            })?;
        Ok(buf[0])
    }

    /// Executes [`AXP192_INIT_SEQUENCE`] over I2C, then verifies reg 0x12
    /// carries the enable bits (the bolty readback guard — a silently
    /// unpowered LCD is indistinguishable from a dead one).
    fn apply_axp192_sequence(i2c: &mut I2cDriver) -> Result<(), &'static str> {
        for op in super::AXP192_INIT_SEQUENCE {
            match op {
                AxpOp::Set { reg, value, why } => {
                    info!("AXP192: reg 0x{reg:02X} = 0x{value:02X} ({why})");
                    axp_write(i2c, *reg, *value)?;
                }
                AxpOp::Or { reg, bits, why } => {
                    let old = axp_read(i2c, *reg)?;
                    info!("AXP192: reg 0x{reg:02X} |= 0x{bits:02X} (was 0x{old:02X}, {why})");
                    axp_write(i2c, *reg, old | bits)?;
                }
            }
        }
        let verify = axp_read(i2c, REG_POWER_OUT_CTL)?;
        if verify & super::AXP192_REG12_ENABLE_BITS != super::AXP192_REG12_ENABLE_BITS {
            warn!(
                "AXP192: power ctrl verify mismatch: 0x{verify:02X} (want bits 0x{:02X})",
                super::AXP192_REG12_ENABLE_BITS
            );
            return Err("axp192 power enable verify failed");
        }
        info!("AXP192: PMU initialized OK (reg 0x12 = 0x{verify:02X})");
        Ok(())
    }

    /// # Safety
    /// Call exactly once at startup: SPI_BUFFER is handed to the display
    /// driver permanently. Claims I2C1 + SPI2 + the nine LCD pins —
    /// none of which the CCID path touches (MFRC522 rides i2c0 on the
    /// Grove port 32/33, UART0 is the GemPC wire).
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn init(
        i2c1: I2C1<'static>,
        spi2: esp_idf_hal::spi::SPI2<'static>,
        pin_sda: Gpio21<'static>,
        pin_scl: Gpio22<'static>,
        pin_sclk: Gpio13<'static>,
        pin_mosi: Gpio15<'static>,
        pin_cs: Gpio5<'static>,
        pin_dc: Gpio23<'static>,
        pin_rst: Gpio18<'static>,
        pin_bl: Gpio27<'static>,
        fwid_rev: &str,
        variant: &'static str,
    ) -> Result<(), &'static str> {
        let mut axp = I2cDriver::new(
            i2c1,
            pin_sda,
            pin_scl,
            &I2cConfig::new().baudrate(400_000.Hz()),
        )
        .map_err(|e| {
            warn!("AXP192 I2C1 init failed: {e:?}");
            "axp192 i2c failed"
        })?;
        apply_axp192_sequence(&mut axp)?;
        info!("AXP192 done, settling power rails 200ms");
        FreeRtos::delay_ms(200);
        if let Ok(mut guard) = I2C1_DRIVER.lock() {
            *guard = Some(axp);
        }

        // 20 MHz write (bolty-proven; M5GFX allows up to 40 MHz — kept
        // conservative for the shared SPI_BUFFER path).
        let spi_driver = SpiDriver::new(
            spi2,
            pin_sclk,
            pin_mosi,
            None::<Gpio12>,
            &DriverConfig::new(),
        )
        .map_err(|e| {
            warn!("SPI2 driver failed: {e:?}");
            "spi2 driver failed"
        })?;
        let spi_device = SpiDeviceDriver::new(
            spi_driver,
            Some(pin_cs),
            &Config::new().baudrate(20.MHz().into()),
        )
        .map_err(|e| {
            warn!("SPI2 device failed: {e:?}");
            "spi2 device failed"
        })?;

        let dc = PinDriver::output(pin_dc).map_err(|e| {
            warn!("DC pin failed: {e:?}");
            "dc pin failed"
        })?;
        let rst = PinDriver::output(pin_rst).map_err(|e| {
            warn!("RST pin failed: {e:?}");
            "rst pin failed"
        })?;
        let mut backlight = PinDriver::output(pin_bl).map_err(|e| {
            warn!("BL pin failed: {e:?}");
            "bl pin failed"
        })?;

        let buffer: &'static mut [u8; SPI_BUFFER_SIZE] =
            unsafe { &mut *core::ptr::addr_of_mut!(SPI_BUFFER) };
        let di = mipidsi::interface::SpiInterface::new(spi_device, dc, buffer);

        let mut display = Builder::new(ST7789, di)
            .reset_pin(rst)
            .display_size(super::LCD_H_RES, super::LCD_V_RES)
            .display_offset(super::DISPLAY_OFFSET_X, super::DISPLAY_OFFSET_Y)
            .invert_colors(ColorInversion::Inverted)
            .init(&mut FreeRtos)
            .map_err(|e| {
                warn!("mipidsi init failed: {e:?}");
                "display init failed"
            })?;

        display.clear(Rgb565::BLACK).map_err(|e| {
            warn!("display clear failed: {e:?}");
            "clear failed"
        })?;
        FreeRtos::delay_ms(50);
        backlight.set_high().map_err(|e| {
            warn!("backlight failed: {e:?}");
            "backlight failed"
        })?;

        info!(
            "ST7789 status screen initialized ({}x{})",
            super::LCD_H_RES,
            super::LCD_V_RES
        );

        let mut guard = SCREEN.lock().map_err(|_| "screen mutex poisoned")?;
        *guard = Some(Screen {
            display,
            data: ScreenData::new(fwid_rev, variant),
            dirty: true,
        });
        Ok(())
    }

    fn with_screen<F: FnOnce(&mut Screen)>(f: F) {
        if let Ok(mut guard) = SCREEN.lock() {
            if let Some(screen) = guard.as_mut() {
                f(screen);
                if screen.dirty {
                    screen.dirty = false;
                    redraw(screen);
                }
            }
        }
    }

    fn clear_line(display: &mut LcdDisplay, y: i32) {
        let _ = Rectangle::new(Point::new(0, y), Size::new(super::LCD_H_RES as u32, 12))
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .fill_color(Rgb565::BLACK)
                    .build(),
            )
            .draw(display);
    }

    fn redraw(screen: &mut Screen) {
        let lines = super::status_lines(&screen.data);
        let white = MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(Rgb565::WHITE)
            .build();
        let green = MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(Rgb565::GREEN)
            .build();
        let yellow = MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(Rgb565::YELLOW)
            .build();

        // Header bar: cyan strip with the FWID line.
        let _ = Rectangle::new(Point::new(0, 0), Size::new(super::LCD_H_RES as u32, 12))
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .fill_color(Rgb565::CSS_DARK_CYAN)
                    .build(),
            )
            .draw(&mut screen.display);
        let bar_style = MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(Rgb565::WHITE)
            .background_color(Rgb565::CSS_DARK_CYAN)
            .build();
        let _ = Text::with_baseline(
            lines[0].as_str(),
            Point::new(2, 0),
            bar_style,
            Baseline::Top,
        )
        .draw(&mut screen.display);

        for (i, (line, style)) in [
            (&lines[1], green),
            (&lines[2], yellow),
            (&lines[3], white),
            (&lines[4], white),
        ]
        .into_iter()
        .enumerate()
        {
            let y = 12 * (i as i32 + 1);
            clear_line(&mut screen.display, y);
            let _ = Text::with_baseline(line.as_str(), Point::new(2, y), style, Baseline::Top)
                .draw(&mut screen.display);
        }
    }

    pub fn set_card_present(present: bool) {
        with_screen(|s| {
            if s.data.card_present != present {
                s.data.card_present = present;
                s.dirty = true;
            }
        });
    }

    /// Pure uptime update (1 Hz, called by the display ticker).
    /// Deliberately I2C-free: bench 2026-10-16 — polling the AXP every
    /// second wedged the legacy-i2c driver at ~4 min uptime and froze
    /// the whole screen while CCID kept serving (the recover_i2c_bus
    /// class, on the AXP bus). The clock must survive a wedged PMU bus.
    pub fn tick(uptime_s: u32) {
        with_screen(|s| {
            if s.data.uptime_s != uptime_s {
                s.data.uptime_s = uptime_s;
                s.dirty = true;
            }
        });
    }

    /// Slow battery refresh (own thread, 10-min cadence): a wedged I2C
    /// bus can at worst stall THIS thread — the battery field goes
    /// stale, the uptime clock does not.
    fn refresh_battery() -> bool {
        let Ok(mut guard) = I2C1_DRIVER.lock() else {
            return false;
        };
        let Some(i2c) = guard.as_mut() else {
            return false;
        };
        let (r78, r79, r01) = (
            axp_read(i2c, REG_BATT_VOLTAGE_H).ok(),
            axp_read(i2c, REG_BATT_VOLTAGE_L).ok(),
            axp_read(i2c, REG_POWER_STATUS).ok(),
        );
        let (Some(r78), Some(r79), Some(r01)) = (r78, r79, r01) else {
            return false;
        };
        let pct = super::battery_pct_from_mv(super::battery_mv_from_regs(r78, r79));
        let usb = super::usb_powered_from_reg01(r01);
        with_screen(|s| {
            s.data.battery_pct = pct;
            s.data.usb_power = usb;
            s.dirty = true;
        });
        true
    }

    /// 1 Hz ticker — owns nothing but the clock; all state flows through
    /// the mutex. Spawned from main; panics here never reach the CCID
    /// serve loop (independent FreeRTOS task). Stack: 12 KB — the tick
    /// path is fmt + embedded-graphics + I2C (bench lesson 2026-10-15:
    /// 4 KB overflowed and reboot-looped the board ~2 s after boot).
    pub fn spawn_ticker() {
        std::thread::Builder::new()
            .stack_size(12288)
            .spawn(|| {
                let mut uptime = 0u32;
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    uptime = uptime.wrapping_add(1);
                    tick(uptime);
                }
            })
            .expect("display ticker spawn");
        // Battery: init read + 10-min cadence; three consecutive failures
        // park the thread (battery "?"-equivalent: shows the last good
        // value) instead of poking a possibly-wedged bus forever.
        std::thread::Builder::new()
            .stack_size(8192)
            .spawn(|| {
                let mut failures = 0u8;
                if !refresh_battery() {
                    failures = 1;
                }
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(600));
                    if failures >= 3 {
                        continue;
                    }
                    if refresh_battery() {
                        failures = 0;
                    } else {
                        failures += 1;
                    }
                }
            })
            .expect("battery refresh spawn");
    }
}

#[cfg(all(target_arch = "xtensa", feature = "board-m5stick", feature = "lcd"))]
pub use imp::{init, set_card_present, spawn_ticker};

// Non-lcd builds: inert no-op stubs so call sites compile unconditionally
// (the led.rs host-stub pattern — the display is absent, not an error).
#[cfg(not(all(target_arch = "xtensa", feature = "board-m5stick", feature = "lcd")))]
mod stub {
    pub fn set_card_present(_present: bool) {}

    pub fn spawn_ticker() {}
}

#[cfg(not(all(target_arch = "xtensa", feature = "board-m5stick", feature = "lcd")))]
pub use stub::{set_card_present, spawn_ticker};

// ---------------------------------------------------------------------------
// Host tests — the GREAT EXPECTATION locks.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// bolty-rs `apps/bolty-esp32/src/display.rs` `init_axp192`
    /// reference sequence, transcribed. Breakage = the sequence drifted
    /// from the bench-proven bring-up — re-verify against the bolty
    /// source and the AXP192 datasheet before changing either side.
    #[test]
    fn axp192_init_sequence_matches_reference() {
        let mut regs: std::vec::Vec<(u8, u8)> = std::vec::Vec::new();
        // reg 0x12 pre-state observed by bolty's read (any value; the
        // sequence ORs into it) — model 0x00 as the neutral base.
        let mut mem: std::collections::HashMap<u8, u8> = std::collections::HashMap::new();
        mem.insert(0x12, 0x00);
        mem.insert(0x10, 0x00);
        for op in AXP192_INIT_SEQUENCE {
            match op {
                AxpOp::Set { reg, value, .. } => {
                    mem.insert(*reg, *value);
                    regs.push((*reg, *value));
                }
                AxpOp::Or { reg, bits, .. } => {
                    let old = *mem.get(reg).unwrap_or(&0);
                    mem.insert(*reg, old | bits);
                    regs.push((*reg, old | bits));
                }
            }
        }
        // The writes, in order (bolty init_axp192):
        assert_eq!(regs[0], (0x28, 0xCC), "LDO2/LDO3 = 3.0V");
        assert_eq!(regs[1], (0x12, 0x4D), "DCDC1+LDO2+LDO3+EXTEN");
        assert_eq!(regs[2], (0x82, 0xFF), "ADC all enabled");
        assert_eq!(regs[3], (0x33, 0xC0), "charge 4.2V/100mA");
        assert_eq!(regs[4], (0x36, 0x0C), "poweron/off timing");
        assert_eq!(regs[5], (0x91, 0xF0), "RTC 3.3V");
        assert_eq!(regs[6], (0x90, 0x02), "GPIO0 LDO mode");
        assert_eq!(regs[7], (0x10, 0x04), "Grove 5V boost");
        // And the verify target after the sequence:
        assert_eq!(
            mem[&0x12] & AXP192_REG12_ENABLE_BITS,
            AXP192_REG12_ENABLE_BITS
        );
    }

    /// M5GFX `Panel_M5StickCPlus` (M5GFX.cpp:475-485): 135x240 at
    /// (52, 40), inverted.
    #[test]
    fn panel_geometry_matches_m5gfx() {
        assert_eq!((LCD_H_RES, LCD_V_RES), (135, 240));
        assert_eq!((DISPLAY_OFFSET_X, DISPLAY_OFFSET_Y), (52, 40));
    }

    /// AXP192 battery decode — datasheet §8.26: raw = r78<<4 | r79>>4,
    /// mv = raw*11/10 (bolty read_battery_mv). Worked example:
    /// r78=0xEC r79=0x70 → raw 3783 → 4161 mV → 96 %.
    #[test]
    fn battery_decode_matches_bolty() {
        assert_eq!(battery_mv_from_regs(0xEC, 0x70), 4161);
        assert_eq!(battery_pct_from_mv(4161), 96);
        assert_eq!(battery_pct_from_mv(3000), 0);
        assert_eq!(battery_pct_from_mv(4200), 100);
        assert!(usb_powered_from_reg01(1 << 5));
        assert!(!usb_powered_from_reg01(0));
    }

    #[test]
    fn status_lines_render_expected_content() {
        let mut d = ScreenData::new("9cdd676", "GemPC serial");
        d.card_present = true;
        d.battery_pct = 77;
        d.usb_power = true;
        d.uptime_s = 3725; // 1h 02m 05s
        let lines = status_lines(&d);
        assert_eq!(lines[0].as_str(), "CCID 9cdd676");
        assert_eq!(lines[1].as_str(), "GemPC serial");
        assert_eq!(lines[2].as_str(), "card: PRESENT");
        assert_eq!(lines[3].as_str(), "batt: 77%+");
        assert_eq!(lines[4].as_str(), "up: 1h02m");
        // No line exceeds the 22-char screen width (135px / 6px glyphs).
        for l in &lines {
            assert!(l.len() <= 22, "line overflows 135px: {l:?}");
        }
    }

    #[test]
    fn status_lines_unknown_battery_and_short_uptime() {
        let d = ScreenData::new("abc", "spp");
        let lines = status_lines(&d);
        assert_eq!(lines[2].as_str(), "card: ---");
        assert_eq!(lines[3].as_str(), "batt: ?");
        assert_eq!(lines[4].as_str(), "up: 0m00s");
    }
}
