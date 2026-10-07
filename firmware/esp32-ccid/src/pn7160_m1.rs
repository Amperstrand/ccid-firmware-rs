//! M1 milestone (issues #63/#80): the absolute minimal Rust main that can
//! probe the PN7160. NOTHING else — no wifi, no netlog, no VEN, no
//! countdown, no OLED pins, no other modules. Isolates "Rust binary on
//! this IDF+driver build" from "our firmware's other init code":
//!   ACK  → something else in our binary poisons the bus; add pieces
//!          back one at a time (M2: +VEN cycle, M3: +NCI, ...)
//!   NAK  → the esp-idf-sys build itself differs from idf.py; the fix is
//!          build-level, not code-level

pub fn run() -> ! {
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::warn!("FWID pn7160-m1 rev={} build={}", env!("FW_GIT_REV"), env!("FW_BUILD_TS"));
    log::warn!("M1v2: bus + VEN + probe (build 18:25)");

    let mut bus: esp_idf_sys::i2c_master_bus_handle_t = core::ptr::null_mut();
    unsafe {
        let mut cfg: esp_idf_sys::i2c_master_bus_config_t = core::mem::zeroed();
        cfg.i2c_port = 0;
        cfg.sda_io_num = 4;
        cfg.scl_io_num = 5;
        cfg.__bindgen_anon_1.clk_source =
            esp_idf_sys::soc_periph_i2c_clk_src_t_I2C_CLK_SRC_DEFAULT;
        cfg.glitch_ignore_cnt = 7;
        let rc = esp_idf_sys::i2c_new_master_bus(&cfg, &mut bus);
        log::warn!("M1: i2c_new_master_bus rc={}", rc);
        if rc != 0 {
            loop {
                esp_idf_sys::esp_rom_delay_us(5_000_000);
            }
        }
    }

        // Wallet-firmware bus priming: the wallet ALWAYS does display probe
    // + keypad transaction BEFORE the PN7160 VEN cycle. The SCL activity
    // from these transactions appears to release the PN7160's I2C slave
    // from power-on reset. Without it, the PN7160 needs ~45 probe attempts
    // (~3.75 minutes) before ACKing. With it: immediate ACK.
    unsafe {
        // OLED probe (will NAK — disconnected — but clocks the bus)
        let oled = esp_idf_sys::i2c_master_probe(bus, 0x3C, 50);
        log::warn!("M1: OLED @0x3C probe rc={} (expected NAK)", oled);

        // Keyboard probe + device add + actual DATA transaction (SCL clocking)
        let kb = esp_idf_sys::i2c_master_probe(bus, 0x20, 50);
        log::warn!("M1: keyboard @0x20 probe rc={}", kb);
        if kb == 0 {
            let mut kdev: esp_idf_sys::i2c_master_dev_handle_t = core::ptr::null_mut();
            let mut kcfg: esp_idf_sys::i2c_device_config_t = core::mem::zeroed();
            kcfg.dev_addr_length = esp_idf_sys::i2c_addr_bit_len_t_I2C_ADDR_BIT_LEN_7;
            kcfg.device_address = 0x20;
            kcfg.scl_speed_hz = 100_000;
            if esp_idf_sys::i2c_master_bus_add_device(bus, &kcfg, &mut kdev) == 0 {
                let idle: [u8; 1] = [0xFF];
                let rc = esp_idf_sys::i2c_master_transmit(kdev, idle.as_ptr(), 1, 100);
                log::warn!("M1: keyboard transmit rc={} (bus primed)", rc);
                esp_idf_sys::i2c_master_bus_rm_device(kdev);
            }
        }
    }

    // M2 test: VEN via PinDriver (HAL) instead of raw gpio — A/B whether
    // the HAL's pin configuration differs from raw gpio_config.
    {
        use esp_idf_hal::gpio::{Output, PinDriver};
        let peripherals = esp_idf_hal::peripherals::Peripherals::take().unwrap();
        let mut ven: PinDriver<'static, Output> = PinDriver::output(peripherals.pins.gpio7).unwrap();
        ven.set_high().unwrap();
        unsafe { esp_idf_sys::vTaskDelay(2); }
        ven.set_low().unwrap();
        unsafe { esp_idf_sys::vTaskDelay(5); }
        ven.set_high().unwrap();
        unsafe { esp_idf_sys::vTaskDelay(5); }
        log::warn!("M2: VEN PinDriver cycle done");
        drop(ven);
        unsafe { esp_idf_sys::vTaskDelay(50); } // 500ms settling
    }

    let mut n: u32 = 0;
    loop {
        unsafe {
            esp_idf_sys::vTaskDelay(500 / 10); // 5s at 100Hz tick
            n += 1;
            let rc = esp_idf_sys::i2c_master_probe(bus, 0x28, 50);
            if rc == 0 {
                log::warn!("M1[{}]: *** PN7160 ACK @0x28 ***", n);
            } else if n <= 5 {
                log::warn!("M1v2[{}]: 0x28 no-ack rc={}", n, rc);
            }
            let kb = esp_idf_sys::i2c_master_probe(bus, 0x20, 50);
            if n <= 3 {
                log::warn!("M1v2[{}]: kb @0x20 rc={}", n, kb);
            }
        }
    }
}
