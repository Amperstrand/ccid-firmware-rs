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

use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{Input, Output, PinDriver, Pull};
use esp_idf_sys::EspError;
use pn7160_nci::transport::{IRQ_PIN, PN7160_I2C_ADDR, SCL_PIN, SDA_PIN, VEN_PIN};
use pn7160_nci::{Frame, Transport, MT_NTF};

use crate::pn7160_driver::Pn7160NfcDriver;

/// Assembled NfcDriver over the real I²C link.
pub type EspPn7160NfcDriver = Pn7160NfcDriver<EspPn7160Transport>;

const XFER_TIMEOUT_MS: i32 = 500;
const IRQ_WAIT_BUDGET_US: u32 = 200_000;
const NTF_SLOTS: usize = 4;

pub(crate) static ISR_SERVICE_RC: core::sync::atomic::AtomicI32 =
    core::sync::atomic::AtomicI32::new(-999);
pub(crate) static ISR_ADD_RC: core::sync::atomic::AtomicI32 =
    core::sync::atomic::AtomicI32::new(-999);

unsafe extern "C" fn dummy_irq_isr(_arg: *mut core::ffi::c_void) {
    // nci.c replica: the ISR must quench the level interrupt itself (the
    // PN7160 holds IRQ high until read); a no-op handler = interrupt storm
    // that starves the console/USB the moment the chip comes alive.
    unsafe { esp_idf_sys::gpio_intr_disable(IRQ_PIN) };
}

pub struct EspPn7160Transport {
    bus: esp_idf_sys::i2c_master_bus_handle_t,
    dev: esp_idf_sys::i2c_master_dev_handle_t,

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
    #[cfg(not(feature = "pn7160-ven-never"))]
    pub fn bringup_config_order(p: BusPins) -> Result<Self, EspError> {
        let mut t = Self::from_peripherals(p)?;
        t.ven_cycle();
        // The PN7160 only ACKs within a window after VEN rise (bench-proven
        // 2026-10-08): probe IMMEDIATELY, like the wallet's nci_init. The
        // device add is bus-silent either way.
        let probe_result = t.probe();
        log::warn!(
            "init: probe @0x28 right after VEN cycle: {:?} (window rule)",
            probe_result
        );
        t.add_pn_device()?;
        Ok(t)
    }

    /// ven-never experiment: assume the chip is ALIVE (wallet handover or
    /// warm board) — VEN raised in µs at construction, NO power cycle.
    #[cfg(feature = "pn7160-ven-never")]
    pub fn bringup_config_order(p: BusPins) -> Result<Self, EspError> {
        let mut t = Self::from_peripherals(p)?;
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
        let _ = p.i2c0; // new driver owns the controller via port number
        let mut bus: esp_idf_sys::i2c_master_bus_handle_t = core::ptr::null_mut();
        let mut dev: esp_idf_sys::i2c_master_dev_handle_t = core::ptr::null_mut();
        unsafe {
            let mut bus_cfg: esp_idf_sys::i2c_master_bus_config_t = core::mem::zeroed();
            bus_cfg.i2c_port = 0;
            bus_cfg.sda_io_num = SDA_PIN as esp_idf_sys::gpio_num_t;
            bus_cfg.scl_io_num = SCL_PIN as esp_idf_sys::gpio_num_t;
            bus_cfg.__bindgen_anon_1.clk_source =
                esp_idf_sys::soc_periph_i2c_clk_src_t_I2C_CLK_SRC_DEFAULT;
            bus_cfg.glitch_ignore_cnt = 7;
            // flags stay zeroed: internal pull-ups OFF (external 2.2k)
            EspError::convert(esp_idf_sys::i2c_new_master_bus(&bus_cfg, &mut bus))?;
            // v10raw-verified sequence (Rust ACK on bench 2026-10-08): raw
            // gpio_config for IRQ + VEN, ISR machinery installed, single clean
            // VEN cycle, probe IMMEDIATELY after. No PinDriver on these pins —
            // the hal wrapper was present in every failing build.
            unsafe {
                let mut irq_cfg: esp_idf_sys::gpio_config_t = core::mem::zeroed();
                irq_cfg.pin_bit_mask = 1u64 << IRQ_PIN;
                irq_cfg.mode = esp_idf_sys::gpio_mode_t_GPIO_MODE_INPUT;
                irq_cfg.pull_down_en = esp_idf_sys::gpio_pulldown_t_GPIO_PULLDOWN_ENABLE;
                irq_cfg.intr_type = esp_idf_sys::gpio_int_type_t_GPIO_INTR_HIGH_LEVEL;
                esp_idf_sys::gpio_config(&irq_cfg);
                ISR_SERVICE_RC.store(
                    esp_idf_sys::gpio_install_isr_service(0),
                    core::sync::atomic::Ordering::Relaxed,
                );
                ISR_ADD_RC.store(
                    esp_idf_sys::gpio_isr_handler_add(
                        IRQ_PIN,
                        Some(dummy_irq_isr),
                        core::ptr::null_mut(),
                    ),
                    core::sync::atomic::Ordering::Relaxed,
                );
                esp_idf_sys::gpio_intr_disable(IRQ_PIN);

                let mut ven_cfg: esp_idf_sys::gpio_config_t = core::mem::zeroed();
                ven_cfg.pin_bit_mask = 1u64 << VEN_PIN;
                ven_cfg.mode = esp_idf_sys::gpio_mode_t_GPIO_MODE_OUTPUT;
                esp_idf_sys::gpio_config(&ven_cfg);
            }
            let _ = p.irq; // raw gpio_config owns IRQ now
            let _ = p.ven; // raw gpio_config owns VEN now
        }
        log::warn!("step: i2c bus + device OK (driver_ng)");
        Ok(Self {
            bus,
            dev,
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
        unsafe { esp_idf_sys::gpio_set_level(VEN_PIN, 1) };
        FreeRtos::delay_ms(10);
        unsafe { esp_idf_sys::gpio_set_level(VEN_PIN, 0) };
        FreeRtos::delay_ms(50);
        unsafe { esp_idf_sys::gpio_set_level(VEN_PIN, 1) };
        FreeRtos::delay_ms(50);
        log::warn!("step: ven_cycle done");
    }

    pub fn ven_cycle_extended(&mut self) {
        unsafe { esp_idf_sys::gpio_set_level(VEN_PIN, 1) };
        FreeRtos::delay_ms(10);
        unsafe { esp_idf_sys::gpio_set_level(VEN_PIN, 0) };
        FreeRtos::delay_ms(100);
        unsafe { esp_idf_sys::gpio_set_level(VEN_PIN, 1) };
        FreeRtos::delay_ms(100);
    }

    /// Two-phase NCI read (nxp-nci style): 3-byte header, then payload.
    fn read_frame(&mut self) -> Option<Frame> {
        let mut hdr = [0u8; 3];
        let rc = unsafe {
            esp_idf_sys::i2c_master_receive(self.dev, hdr.as_mut_ptr(), 3, XFER_TIMEOUT_MS)
        };
        if rc != 0 {
            log::error!("read_frame: header rc={} (chip mute mid-ladder?)", rc);
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
                log::error!("read_frame: payload rc={} plen={}", rc, plen);
                return None;
            }
        }
        {
            let show = (3 + plen).min(9);
            log::warn!(
                "read_frame: hdr={:#04x} {:#04x} plen={} bytes={:?}",
                hdr[0],
                hdr[1],
                plen,
                &pkt[..show]
            );
        }
        Frame::decode(&pkt[..3 + plen])
    }

    /// Single-address health check (issue #63): zero-length probe via the
    /// new driver's dedicated API. Ok(()) = chip ACKs (powered and off the
    /// DWL boot), Err = NAK/timeout (chip mute).
    pub fn probe(&mut self) -> Result<(), EspError> {
        self.probe_addr(PN7160_I2C_ADDR)
    }

    /// Probe an arbitrary address (address-strap margin diagnostic:
    /// nucula-board R23/R24 = 100k vs internal pull-ups → the chip may
    /// land on 0x29-0x2B instead of 0x28 at VEN rise).
    pub fn probe_addr(&mut self, addr: u8) -> Result<(), EspError> {
        EspError::convert(unsafe { esp_idf_sys::i2c_master_probe(self.bus, addr as u16, 50) })
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
            if unsafe { esp_idf_sys::gpio_get_level(IRQ_PIN) } == 1 {
                let f = self.read_frame()?;
                // The reply to what we sent is the first NON-NTF frame:
                // MT_RSP for commands, MT_DATA for DATA packets. Returning
                // only MT_RSP stashed every APDU response as a notification
                // and starved all card exchanges (mock/hardware divergence
                // — the mock returns any queued reply; bench 2026-10-08).
                if f.mt != MT_NTF {
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
            // FreeRtos (not Ets) — the ROM busy-wait starves the USB
            // console task for the whole ladder: console output dies
            // while the app + chip keep working (AGENTS.md hazard).
            FreeRtos::delay_ms(1);
            waited += 1000;
        }
    }

    fn drain(&mut self) -> Option<Frame> {
        if let Some(f) = self.pop_ntf() {
            return Some(f);
        }
        if unsafe { esp_idf_sys::gpio_get_level(IRQ_PIN) } == 1 {
            self.read_frame()
        } else {
            None
        }
    }
}
