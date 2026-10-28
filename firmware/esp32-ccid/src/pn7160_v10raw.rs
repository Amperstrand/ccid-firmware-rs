//! C-control v10 ported to raw syscalls — the minimal ACKing sequence
//! (bench-proven in C): bus → IRQ cfg + ISR install → VEN cfg → VEN cycle
//! → immediate probe. NO netlog, NO Peripherals::take, NO PinDriver, no
//! logging between the hardware steps (exactly like the C binary). If this
//! ACKs where the full Rust bringup fails, the culprit is one of the
//! removed Rust layers — add them back one at a time.

use boards::{BusRole, NUCULA};
use esp_idf_sys as sys;

// nci.c replica: the ISR must quench the level interrupt itself (the
// PN7160 holds IRQ high until read); a no-op handler = interrupt storm
// that starves the console/USB the moment the chip comes alive — on a
// warm reset IRQ can already be high when the handler is added (Codex
// #85, issue #112). Same contract as dummy_irq_isr in pn7160_i2c.rs.
unsafe extern "C" fn dummy_isr(_arg: *mut core::ffi::c_void) {
    unsafe { sys::gpio_intr_disable(IRQ) };
}

/// C's `vTaskDelay(ms / portTICK_PERIOD_MS)`: FreeRTOS task.h defines
/// pdMS_TO_TICKS(ms) = ms * configTICK_RATE_HZ / 1000, but bindgen does
/// not export the macro — expand it against the exported
/// configTICK_RATE_HZ constant (=100 here). Raw vTaskDelay(10) at
/// CONFIG_FREERTOS_HZ=100 sleeps 100 ms, not 10 ms (Codex #85, issue #112).
fn delay_ticks(ms: u32) -> sys::TickType_t {
    ms * sys::configTICK_RATE_HZ / 1000
}

// boards-as-data (t_a0ebcf54): the raw gpio numbers resolve from the
// GENERATED boards::NUCULA row at COMPILE time. The C-replica's
// fidelity is the syscall SEQUENCE (unchanged below); the pin values
// it drives are netlist facts, not hand-typed copies.
const SDA: i32 = NUCULA.bus_i2c_pins_const(BusRole::NfcFrontend).0 as i32;
const SCL: i32 = NUCULA.bus_i2c_pins_const(BusRole::NfcFrontend).1 as i32;
const IRQ: i32 = NUCULA.pin_const("nfc_irq") as i32;
const VEN: i32 = NUCULA.pin_const("nfc_ven") as i32;

pub fn run() -> ! {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::warn!(
        "FWID pn7160-v10raw rev={} build={}",
        env!("FW_GIT_REV"),
        env!("FW_BUILD_TS")
    );
    log::warn!("v10raw: bus + irq/isr + ven + cycle + immediate probe");

    unsafe {
        let mut bus: sys::i2c_master_bus_handle_t = core::ptr::null_mut();
        let mut bus_cfg: sys::i2c_master_bus_config_t = core::mem::zeroed();
        bus_cfg.i2c_port = 0;
        bus_cfg.sda_io_num = SDA as sys::gpio_num_t;
        bus_cfg.scl_io_num = SCL as sys::gpio_num_t;
        bus_cfg.__bindgen_anon_1.clk_source = sys::soc_periph_i2c_clk_src_t_I2C_CLK_SRC_DEFAULT;
        bus_cfg.glitch_ignore_cnt = 7;
        let rc_bus = sys::i2c_new_master_bus(&bus_cfg, &mut bus);
        log::warn!("v10raw: bus rc={}", rc_bus);
        if rc_bus != 0 {
            loop {
                sys::vTaskDelay(delay_ticks(5000));
            }
        }

        // nci.c:52-66
        let mut irq_cfg: sys::gpio_config_t = core::mem::zeroed();
        irq_cfg.pin_bit_mask = 1u64 << IRQ;
        irq_cfg.mode = sys::gpio_mode_t_GPIO_MODE_INPUT;
        irq_cfg.pull_down_en = sys::gpio_pulldown_t_GPIO_PULLDOWN_ENABLE;
        irq_cfg.intr_type = sys::gpio_int_type_t_GPIO_INTR_HIGH_LEVEL;
        let rc_irq = sys::gpio_config(&irq_cfg);
        let rc_svc = sys::gpio_install_isr_service(0);
        let rc_add = sys::gpio_isr_handler_add(IRQ, Some(dummy_isr), core::ptr::null_mut());
        sys::gpio_intr_disable(IRQ);
        log::warn!("v10raw: irq cfg={} svc={} add={}", rc_irq, rc_svc, rc_add);

        // nci.c:67-76 VEN output
        let mut ven_cfg: sys::gpio_config_t = core::mem::zeroed();
        ven_cfg.pin_bit_mask = 1u64 << VEN;
        ven_cfg.mode = sys::gpio_mode_t_GPIO_MODE_OUTPUT;
        let rc_ven = sys::gpio_config(&ven_cfg);
        log::warn!("v10raw: ven cfg rc={}", rc_ven);

        // nci.c:77-86 VEN cycle — delays via vTaskDelay, no logging inside
        sys::gpio_set_level(VEN, 1);
        sys::vTaskDelay(delay_ticks(10));
        sys::gpio_set_level(VEN, 0);
        sys::vTaskDelay(delay_ticks(50));
        sys::gpio_set_level(VEN, 1);
        sys::vTaskDelay(delay_ticks(50));

        // nci.c:90 — probe IMMEDIATELY after the cycle
        let rc_probe = sys::i2c_master_probe(bus, 0x28, 50);
        log::warn!(
            "v10raw: IMMEDIATE probe rc={} ({})",
            rc_probe,
            if rc_probe == 0 { "ACK !!!" } else { "no-ack" }
        );

        let mut hb: u32 = 0;
        loop {
            sys::vTaskDelay(delay_ticks(5000));
            hb += 1;
            let rc = sys::i2c_master_probe(bus, 0x28, 50);
            log::warn!("v10raw[{}]: probe rc={}", hb, rc);
        }
    }
}
