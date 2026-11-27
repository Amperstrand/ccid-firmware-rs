//! #547 rf-dark idle main: the nucula's PN7160 held VEN-LOW — an
//! OTA-reachable "no RF" state for bench orchestration (the RF-quiet
//! window the ACR1252 ↔ nucula-listener legs need, without hands).
//!
//! Semantics (design notes, issue #547):
//! - brings the transport up far enough to OWN the VEN pad, then holds
//!   it low: no discovery, no field, I2C dark;
//! - NEVER marks the OTA image valid (`ota_validity` untouched) — on
//!   reset the bootloader's rollback returns the serving image: RESET
//!   IS RF_RESUME;
//! - never claims the CCID wire (no serving): console heartbeat is the
//!   only output, one line a minute, FWID-marked.
//! Feature: `pn7160-rfdark` (build.sh board `pn7160-rfdark`).

use esp_idf_hal::delay::FreeRtos;

use crate::board_ctor;

pub fn run() -> ! {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::warn!(
        "FWID pn7160-rfdark rev={} build={}",
        env!("FW_GIT_REV"),
        env!("FW_BUILD_TS")
    );
    log::warn!("rf-dark: taking the VEN pad, then the field goes quiet");

    let peripherals = esp_idf_hal::peripherals::Peripherals::take().expect("peripherals");
    let bus = board_ctor::pn7160::nucula_bus_pins(peripherals.i2c0, peripherals.pins);
    let pins = board_ctor::pn7160::binding_from_board(&boards::NUCULA);
    let mut transport =
        crate::pn7160_i2c::EspPn7160Transport::bringup_config_order(pins, bus).expect("transport");
    transport.ven_hold_low();

    let mut tick: u32 = 0;
    loop {
        FreeRtos::delay_ms(60_000);
        tick += 1;
        // One line a minute: presence + state proof for the bench
        // (console-only; this main never serves a CCID wire).
        log::warn!("rf-dark: hb {} — VEN low, no RF (reset = resume)", tick);
    }
}
