//! Concrete PN7160 transport for esp-idf targets: NCI over I²C (0x28)
//! with VEN power control and IRQ-driven reads — the nci_transceive /
//! nci_read split of the proven C driver. Uses the NEW `i2c_master`
//! driver via raw esp-idf-sys bindings (issue #63/#80): the PN7160 is
//! permanently mute under the legacy `driver/i2c` on this silicon, and
//! ESP-IDF aborts when both driver generations are linked — hence the
//! vendored hal patch that gates the legacy module out. The three
//! bring-up constructors are the pad-diag verdict map (issue #62):
//! A = pad-hold clear, B = config order (baseline), C = extended VEN
//! timing.

use esp_idf_hal::delay::{Ets, FreeRtos};
use esp_idf_hal::gpio::{Input, Output, PinDriver, Pull};
use esp_idf_sys::EspError;
use pn7160_nci::transport::{IRQ_PIN, PN7160_I2C_ADDR, SCL_PIN, SDA_PIN, VEN_PIN};
use pn7160_nci::{Frame, Transport, MT_RSP};

use crate::pn7160_driver::Pn7160NfcDriver;

/// Assembled NfcDriver over the real I²C link.
pub type EspPn7160NfcDriver = Pn7160NfcDriver<EspPn7160Transport>;

const XFER_TIMEOUT_MS: i32 = 500;
const IRQ_POLL_US: u32 = 250;
const IRQ_WAIT_BUDGET_US: u32 = 200_000;
const NTF_SLOTS: usize = 4;

pub struct EspPn7160Transport {
    bus: esp_idf_sys::i2c_master_bus_handle_t,
    dev: esp_idf_sys::i2c_master_dev_handle_t,
    ven: PinDriver<'static, Output>,
    irq: PinDriver<'static, Input>,
    ntf: [Option<Frame>; NTF_SLOTS],
    ntf_head: usize,
    ntf_count: usize,
}

/// Decomposed board pins for the NFC block: the bring-up main splits
/// `Peripherals` once (modem + nvs to WiFi, the rest here) because
/// `Peripherals::take` is single-shot.
pub struct BusPins {
    pub i2c0: esp_idf_hal::i2c::I2C0<'static>,
    pub sda: esp_idf_hal::gpio::Gpio4<'static>,
    pub scl: esp_idf_hal::gpio::Gpio5<'static>,
    pub irq: esp_idf_hal::gpio::Gpio6<'static>,
    pub ven: esp_idf_hal::gpio::Gpio7<'static>,
}

impl EspPn7160Transport {
    /// Verdict B (baseline): I2C bus first — the native firmware's
    /// nucula.cpp:181 position — then the VEN power cycle.
    pub fn bringup_config_order(p: BusPins) -> Result<Self, EspError> {
        let mut t = Self::from_peripherals(p)?;
        t.ven_cycle();
        t.add_pn_device()?;
        Ok(t)
    }

    /// nci.c adds the PN7160 device only after probing post-VEN; we mirror
    /// that (device-add is bus-silent, but ordering is replicated exactly).
    fn add_pn_device(&mut self) -> Result<(), EspError> {
        if !self.dev.is_null() {
            return Ok(());
        }
        let mut dev_cfg: esp_idf_sys::i2c_device_config_t = unsafe { core::mem::zeroed() };
        dev_cfg.dev_addr_length = esp_idf_sys::i2c_addr_bit_len_t_I2C_ADDR_BIT_LEN_7;
        dev_cfg.device_address = PN7160_I2C_ADDR as u16;
        dev_cfg.scl_speed_hz = 100_000;
        unsafe {
            EspError::convert(esp_idf_sys::i2c_master_bus_add_device(
                self.bus,
                &dev_cfg,
                &mut self.dev,
            ))?;
        }
        Ok(())
    }

    /// Verdict A (pads stuck): clear gpio hold + sleep isolation on the
    /// four NFC pads before reconfiguration. A latched hold ignores
    /// config changes until cleared (cf. Tasmota #20030).
    pub fn bringup_pad_hold_clear(p: BusPins) -> Result<Self, EspError> {
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
    pub fn bringup_ven_timing(p: BusPins) -> Result<Self, EspError> {
        let mut t = Self::from_peripherals(p)?;
        t.ven_cycle_extended();
        t.add_pn_device()?;
        Ok(t)
    }

    fn from_peripherals(p: BusPins) -> Result<Self, EspError> {
        // Board rules (zeugmaster/nucula-board peripherals-design.md):
        // 100 kHz max (PCF8574T limit), external 2.2k pulls to 3.0 V —
        // ESP internal pull-ups must stay OFF.
        //
        // Board bring-up the wallet firmware performs before NFC init and
        // we must mirror (root cause of the #63/#80 "mute PN7160"): the
        // unpopulated SSD1309 OLED's boost gate (GPIO3) and reset (GPIO10)
        // have to be driven LOW/HIGH. Left floating, the partially-powered
        // OLED leaks through its ESD diodes onto SDA/SCL — phantom stride-3
        // ACKs across the bus scan and the PN7160's real ACK at 0x28 sunk
        // below the driver's detection threshold.
        unsafe {
            esp_idf_sys::gpio_set_level(3, 0); // OLED boost OFF
            esp_idf_sys::gpio_set_direction(
                3,
                esp_idf_sys::gpio_mode_t_GPIO_MODE_OUTPUT,
            );
            esp_idf_sys::gpio_set_level(10, 1); // SSD1309 reset asserted
            esp_idf_sys::gpio_set_direction(
                10,
                esp_idf_sys::gpio_mode_t_GPIO_MODE_OUTPUT,
            );
        }
        let irq: PinDriver<'static, Input> = PinDriver::input(p.irq, Pull::Down)?;
        let ven: PinDriver<'static, Output> = PinDriver::output(p.ven)?;
        let _ = p.i2c0; // new driver owns the controller via port number
        let mut bus: esp_idf_sys::i2c_master_bus_handle_t = core::ptr::null_mut();
        let mut dev: esp_idf_sys::i2c_master_dev_handle_t = core::ptr::null_mut();
        unsafe {
            let mut bus_cfg: esp_idf_sys::i2c_master_bus_config_t = core::mem::zeroed();
            bus_cfg.i2c_port = 0;
            bus_cfg.sda_io_num = SDA_PIN as esp_idf_sys::gpio_num_t;
            bus_cfg.scl_io_num = SCL_PIN as esp_idf_sys::gpio_num_t;
            bus_cfg.__bindgen_anon_1.clk_source = esp_idf_sys::soc_periph_i2c_clk_src_t_I2C_CLK_SRC_DEFAULT;
            bus_cfg.glitch_ignore_cnt = 7;
            // flags stay zeroed: internal pull-ups OFF (external 2.2k)
            EspError::convert(esp_idf_sys::i2c_new_master_bus(&bus_cfg, &mut bus))?;

            // Wallet-replica bus priming (nci.c + keypad.c ordering): the
            // ONLY pre-VEN I2C traffic the wallet generates is one keypad
            // probe + device-add + transmit to the PCF8574 at 0x20. The
            // wallet's display_init is a stub (GPIO quiesce only, returns
            // before any 0x3C probe) — probing the unpowered OLED's ESD
            // leak path disturbs the bus, so we must NOT send 0x3C.
            // The PN7160 device is also added only AFTER the VEN cycle
            // (nci.c adds it post-probe); we mirror that ordering.
            if esp_idf_sys::i2c_master_probe(bus, 0x20, 50) == 0 {
                let mut kdev: esp_idf_sys::i2c_master_dev_handle_t = core::ptr::null_mut();
                let mut kcfg: esp_idf_sys::i2c_device_config_t = core::mem::zeroed();
                kcfg.dev_addr_length = esp_idf_sys::i2c_addr_bit_len_t_I2C_ADDR_BIT_LEN_7;
                kcfg.device_address = 0x20;
                kcfg.scl_speed_hz = 100_000;
                if esp_idf_sys::i2c_master_bus_add_device(bus, &kcfg, &mut kdev) == 0 {
                    let idle: [u8; 1] = [0xFF];
                    let rc = esp_idf_sys::i2c_master_transmit(kdev, idle.as_ptr(), 1, 100);
                    log::warn!("keypad-replica: transmit rc={}", rc);
                    esp_idf_sys::i2c_master_bus_rm_device(kdev);
                }
            } else {
                log::warn!("keypad-replica: no PCF8574 @0x20");
            }
        }
        log::warn!("step: i2c bus + device OK (driver_ng)");
        Ok(Self {
            bus,
            dev,
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

    /// VEN power cycle, byte-exact vs nci.c:77-86. Public: the bring-up
    /// health loop re-applies it before every init-ladder retry.
    pub fn ven_cycle(&mut self) {
        log::warn!("step: ven_cycle begin");
        let _ = self.ven.set_high();
        FreeRtos::delay_ms(10);
        let _ = self.ven.set_low();
        FreeRtos::delay_ms(50);
        let _ = self.ven.set_high();
        FreeRtos::delay_ms(50);
        log::warn!("step: ven_cycle done");
    }

    pub fn ven_cycle_extended(&mut self) {
        let _ = self.ven.set_high();
        FreeRtos::delay_ms(10);
        let _ = self.ven.set_low();
        FreeRtos::delay_ms(100);
        let _ = self.ven.set_high();
        FreeRtos::delay_ms(100);
    }

    /// Two-phase NCI read (nxp-nci style): 3-byte header, then payload.
    fn read_frame(&mut self) -> Option<Frame> {
        let mut hdr = [0u8; 3];
        let rc = unsafe {
            esp_idf_sys::i2c_master_receive(self.dev, hdr.as_mut_ptr(), 3, XFER_TIMEOUT_MS)
        };
        if rc != 0 {
            return None;
        }
        let plen = hdr[2] as usize;
        let mut pkt = [0u8; 3 + 255];
        pkt[..3].copy_from_slice(&hdr);
        if plen > 0 {
            let rc = unsafe {
                esp_idf_sys::i2c_master_receive(
                    self.dev,
                    pkt[3..3 + plen].as_mut_ptr(),
                    plen,
                    XFER_TIMEOUT_MS,
                )
            };
            if rc != 0 {
                return None;
            }
        }
        Frame::decode(&pkt[..3 + plen])
    }

    /// Single-address health check (issue #63): zero-length probe via the
    /// new driver's dedicated API. Ok(()) = chip ACKs (powered and off the
    /// DWL boot), Err = NAK/timeout (chip mute).
    pub fn probe(&mut self) -> Result<(), EspError> {
        EspError::convert(unsafe {
            esp_idf_sys::i2c_master_probe(self.bus, PN7160_I2C_ADDR as u16, 50)
        })
    }

    /// Probe 0x01..=0x7F; log every ACK and the error kind at the PN7160
    /// address when it stays mute.
    pub fn i2c_scan(&mut self) {
        let mut found = 0u32;
        for addr in 1u16..=0x7F {
            let rc = unsafe { esp_idf_sys::i2c_master_probe(self.bus, addr, 50) };
            if rc == 0 {
                log::warn!("i2c scan: ACK at 0x{:02X}", addr);
                found += 1;
            } else if addr as u8 == PN7160_I2C_ADDR {
                log::error!("i2c scan: 0x{:02X} err: {}", PN7160_I2C_ADDR, rc);
            }
        }
        log::warn!("i2c scan done: {} responders", found);
    }
}

impl Transport for EspPn7160Transport {
    fn transact(&mut self, cmd: &[u8]) -> Option<Frame> {
        let rc = unsafe {
            esp_idf_sys::i2c_master_transmit(self.dev, cmd.as_ptr(), cmd.len(), XFER_TIMEOUT_MS)
        };
        if rc != 0 {
            log::error!("transact: i2c write err: {}", rc);
            return None;
        }
        let mut waited = 0u32;
        loop {
            if self.irq.is_high() {
                let f = self.read_frame()?;
                if f.mt == MT_RSP {
                    return Some(f);
                }
                log::warn!("transact: stashing NTF gid={:#x} oid={:#x}", f.gid, f.oid);
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
