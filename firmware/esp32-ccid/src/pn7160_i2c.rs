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
use pn7160_nci::{Frame, Transport, MT_RSP};

use crate::pn7160_driver::Pn7160NfcDriver;

/// Assembled NfcDriver over the real I2C link.
pub type EspPn7160NfcDriver = Pn7160NfcDriver<EspPn7160Transport>;

const I2C_TIMEOUT_TICKS: u32 = 100;
const IRQ_POLL_US: u32 = 250;
const IRQ_WAIT_BUDGET_US: u32 = 200_000;
const NTF_SLOTS: usize = 4;

pub struct EspPn7160Transport {
    i2c: I2cDriver<'static>,
    ven: PinDriver<'static, Output>,
    irq: PinDriver<'static, Input>,
    ntf: [Option<Frame>; NTF_SLOTS],
    ntf_head: usize,
    ntf_count: usize,
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
        // Board rules (zeugmaster/nucula-board peripherals-design.md):
        // 100 kHz max (PCF8574T limit), external 2.2k pulls to 3.0 V —
        // ESP internal pull-ups must stay OFF.
        let config = I2cConfig::new()
            .baudrate(Hertz(100_000).into())
            .sda_enable_pullup(false)
            .scl_enable_pullup(false);
        log::warn!("step: I2cDriver::new...");
        let i2c = I2cDriver::new(p.i2c0, p.pins.gpio4, p.pins.gpio5, &config)?;
        log::warn!("step: i2c driver OK");
        let ven: PinDriver<'static, Output> = PinDriver::output(p.pins.gpio7)?;
        log::warn!("step: ven pin OK");
        let irq: PinDriver<'static, Input> = PinDriver::input(p.pins.gpio6, Pull::Down)?;
        log::warn!("step: irq pin OK");
        Ok(Self {
            i2c,
            ven,
            irq,
            ntf: [None; NTF_SLOTS],
            ntf_head: 0,
            ntf_count: 0,
        })
    }

    fn push_ntf(&mut self, f: Frame) {
        if self.ntf_count == NTF_SLOTS {
            self.ntf[self.ntf_head] = None;
            self.ntf_head = (self.ntf_head + 1) % NTF_SLOTS;
            self.ntf_count -= 1;
        }
        let tail = (self.ntf_head + self.ntf_count) % NTF_SLOTS;
        self.ntf[tail] = Some(f);
        self.ntf_count += 1;
    }

    fn pop_ntf(&mut self) -> Option<Frame> {
        if self.ntf_count == 0 {
            return None;
        }
        let f = self.ntf[self.ntf_head].take();
        self.ntf_head = (self.ntf_head + 1) % NTF_SLOTS;
        self.ntf_count -= 1;
        f
    }

    /// VEN power cycle, byte-exact vs nci.c:77-86.
    fn ven_cycle(&mut self) {
        log::warn!("step: ven_cycle begin");
        let _ = self.ven.set_high();
        Ets::delay_us(10_000);
        let _ = self.ven.set_low();
        Ets::delay_us(50_000);
        let _ = self.ven.set_high();
        Ets::delay_us(50_000);
        log::warn!("step: ven_cycle done");
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
        use std::sync::atomic::{AtomicU8, Ordering};
        static READ_LOG: AtomicU8 = AtomicU8::new(0);
        let verbose = READ_LOG.fetch_add(1, Ordering::Relaxed) < 8;
        let mut hdr = [0u8; 3];
        self.i2c
            .read(PN7160_I2C_ADDR, &mut hdr, I2C_TIMEOUT_TICKS)
            .ok()?;
        if verbose {
            log::warn!("rf hdr: {:02x?}", hdr);
        }
        let plen = hdr[2] as usize;
        let mut pkt = [0u8; 3 + 255];
        pkt[..3].copy_from_slice(&hdr);
        self.i2c
            .read(PN7160_I2C_ADDR, &mut pkt[3..3 + plen], I2C_TIMEOUT_TICKS)
            .ok()?;
        if verbose {
            log::warn!("rf body[{}]: {:02x?}", plen, &pkt[3..3 + plen.min(16)]);
        }
        Frame::decode(&pkt[..3 + plen])
    }

    /// Probe 0x01..=0x7F with zero-length writes; log every ACK and
    /// the error kind at the PN7160 address when it stays mute.
    pub fn i2c_scan(&mut self) {
        let mut found = 0u32;
        for addr in 1u8..=0x7F {
            match self.i2c.write(addr, &[], 1) {
                Ok(()) => {
                    log::warn!("i2c scan: ACK at 0x{:02X}", addr);
                    found += 1;
                }
                Err(e) if addr == PN7160_I2C_ADDR => {
                    log::error!("i2c scan: 0x{:02X} err: {:?}", PN7160_I2C_ADDR, e);
                }
                Err(_) => {}
            }
        }
        log::warn!("i2c scan done: {} responders", found);
    }
}

impl Transport for EspPn7160Transport {
    fn transact(&mut self, cmd: &[u8]) -> Option<Frame> {
        if let Err(e) = self.i2c.write(PN7160_I2C_ADDR, cmd, I2C_TIMEOUT_TICKS) {
            log::error!("transact: i2c write err: {:?}", e);
            return None;
        }
        let mut waited = 0u32;
        loop {
            if self.irq.is_high() {
                let f = self.read_frame()?;
                if f.mt == MT_RSP {
                    return Some(f);
                }
                log::warn!(
                    "transact: stashing NTF gid={:#x} oid={:#x}",
                    f.gid,
                    f.oid
                );
                self.push_ntf(f);
                continue;
            }
            if waited >= IRQ_WAIT_BUDGET_US {
                log::error!("transact: RSP timeout");
                return None;
            }
            Ets::delay_us(IRQ_POLL_US);
            waited += IRQ_POLL_US;
        }
    }

    fn drain(&mut self) -> Option<Frame> {
        if let Some(f) = self.pop_ntf() {
            return Some(f);
        }
        if self.irq.is_high() {
            self.read_frame()
        } else {
            None
        }
    }
}
