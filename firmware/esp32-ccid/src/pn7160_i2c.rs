//! Concrete PN7160 transport for esp-idf targets: NCI over I2C (0x28)
//! with VEN power control and IRQ-driven reads — the nci_transceive /
//! nci_read split of the proven C driver. The three bring-up
//! constructors are the pad-diag verdict map (issue #62): A = pad-hold
//! clear, B = config order (baseline), C = extended VEN timing.

use esp_idf_hal::delay::Ets;
use esp_idf_hal::gpio::{Input, Output, PinDriver, Pull};
use esp_idf_hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::units::Hertz;
use esp_idf_sys::EspError;

use pn7160_nci::transport::{IRQ_PIN, PN7160_I2C_ADDR, SCL_PIN, SDA_PIN, VEN_PIN};
use pn7160_nci::{Frame, Transport};

use crate::pn7160_driver::Pn7160NfcDriver;

/// Assembled NfcDriver over the real I2C link.
pub type EspPn7160NfcDriver = Pn7160NfcDriver<EspPn7160Transport>;

const I2C_TIMEOUT_TICKS: u32 = 100;
const IRQ_POLL_US: u32 = 250;
const IRQ_WAIT_BUDGET_US: u32 = 200_000;

pub struct EspPn7160Transport {
    i2c: I2cDriver<'static>,
    ven: PinDriver<'static, Output>,
    irq: PinDriver<'static, Input>,
}

impl EspPn7160Transport {
    /// Verdict B (baseline): I2C bus first — the native firmware's
    /// nucula.cpp:181 position — then the VEN power cycle.
    pub fn bringup_config_order(p: Peripherals) -> Result<Self, EspError> {
        let mut t = Self::from_peripherals(p)?;
        t.ven_cycle();
        Ok(t)
    }

    /// Verdict A (pads stuck): clear gpio hold + sleep isolation on the
    /// four NFC pads before reconfiguration. A latched hold ignores
    /// config changes until cleared (cf. Tasmota #20030).
    pub fn bringup_pad_hold_clear(p: Peripherals) -> Result<Self, EspError> {
        unsafe {
            esp_idf_sys::gpio_deep_sleep_hold_dis();
            for pin in [SDA_PIN, SCL_PIN, IRQ_PIN, VEN_PIN] {
                esp_idf_sys::gpio_hold_dis(pin);
                esp_idf_sys::gpio_sleep_sel_dis(pin);
            }
        }
        Self::bringup_config_order(p)
    }

    /// Verdict C: quiesce the core with extended VEN timing.
    pub fn bringup_ven_timing(p: Peripherals) -> Result<Self, EspError> {
        let mut t = Self::from_peripherals(p)?;
        t.ven_cycle_extended();
        Ok(t)
    }

    fn from_peripherals(p: Peripherals) -> Result<Self, EspError> {
        let config = I2cConfig::new().baudrate(Hertz(400_000).into());
        let i2c = I2cDriver::new(p.i2c0, p.pins.gpio4, p.pins.gpio5, &config)?;
        let ven: PinDriver<'static, Output> = PinDriver::output(p.pins.gpio7)?;
        let irq: PinDriver<'static, Input> = PinDriver::input(p.pins.gpio6, Pull::Floating)?;
        Ok(Self { i2c, ven, irq })
    }

    /// VEN power cycle, byte-exact vs nci.c:77-86.
    fn ven_cycle(&mut self) {
        let _ = self.ven.set_high();
        Ets::delay_us(10_000);
        let _ = self.ven.set_low();
        Ets::delay_us(50_000);
        let _ = self.ven.set_high();
        Ets::delay_us(50_000);
    }

    fn ven_cycle_extended(&mut self) {
        let _ = self.ven.set_high();
        Ets::delay_us(10_000);
        let _ = self.ven.set_low();
        Ets::delay_us(100_000);
        let _ = self.ven.set_high();
        Ets::delay_us(100_000);
    }

    /// Two-phase NCI read (nxp-nci style): 3-byte header, then payload.
    fn read_frame(&mut self) -> Option<Frame> {
        let mut hdr = [0u8; 3];
        self.i2c
            .read(PN7160_I2C_ADDR, &mut hdr, I2C_TIMEOUT_TICKS)
            .ok()?;
        let plen = hdr[2] as usize;
        let mut pkt = [0u8; 3 + 255];
        pkt[..3].copy_from_slice(&hdr);
        self.i2c
            .read(PN7160_I2C_ADDR, &mut pkt[3..3 + plen], I2C_TIMEOUT_TICKS)
            .ok()?;
        Frame::decode(&pkt[..3 + plen])
    }
}

impl Transport for EspPn7160Transport {
    fn transact(&mut self, cmd: &[u8]) -> Option<Frame> {
        self.i2c
            .write(PN7160_I2C_ADDR, cmd, I2C_TIMEOUT_TICKS)
            .ok()?;
        let mut waited = 0u32;
        while !self.irq.is_high() {
            if waited >= IRQ_WAIT_BUDGET_US {
                return None;
            }
            Ets::delay_us(IRQ_POLL_US);
            waited += IRQ_POLL_US;
        }
        self.read_frame()
    }

    fn drain(&mut self) -> Option<Frame> {
        if self.irq.is_high() {
            self.read_frame()
        } else {
            None
        }
    }
}
