//! Activation diagnostic main: full NCI chain with console logging at
//! every step (no USB-CDC driver claim — logs stay alive). Isolates the
//! power_on failure: discovery NTF → RF_DISCOVER_SELECT → activation NTF
//! → ATS → ATR. Feature: `pn7160-actdiag`.

use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::peripherals::Peripherals;

use esp_idf_sys::link_patches;

use crate::nfc::NfcDriver;
use crate::pn7160_driver::Pn7160NfcDriver;
use crate::pn7160_i2c::{BusPins, EspPn7160Transport};
use pn7160_nci::reader;
use pn7160_nci::{Frame, Transport};

fn show(f: &Frame) -> String {
    format!(
        "mt={:#04x} gid={:#x} oid={:#x} len={} payload={:?}",
        f.mt,
        f.gid,
        f.oid,
        f.len,
        &f.payload[..f.len.min(12)]
    )
}

pub fn run() -> ! {
    link_patches();
    esp_idf_hal::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::warn!(
        "FWID pn7160-actdiag rev={} build={}",
        env!("FW_GIT_REV"),
        env!("FW_BUILD_TS")
    );

    let peripherals = Peripherals::take().expect("peripherals");
    let bus = BusPins {
        i2c0: peripherals.i2c0,
        sda: peripherals.pins.gpio4,
        scl: peripherals.pins.gpio5,
        irq: peripherals.pins.gpio6,
        ven: peripherals.pins.gpio7,
    };
    let mut transport = EspPn7160Transport::bringup_config_order(bus).expect("transport");
    let mut d = Pn7160NfcDriver::new(transport);
    d.init().expect("ladder");
    log::warn!("actdiag: ladder OK — waiting for discovery NTF (watch 30s)");

    // raw discovery wait with full logging
    let mut ntf = None;
    for round in 1..=60u32 {
        FreeRtos::delay_ms(500);
        if let Some(n) = reader::wait_for_discovery(d.transport_mut()) {
            log::warn!("actdiag: NTF after {} rounds: {:?}", round, n);
            ntf = Some(n);
            break;
        }
        if round % 10 == 0 {
            log::warn!("actdiag: still no NTF (round {})", round);
        }
    }
    let ntf = match ntf {
        Some(n) => n,
        None => {
            log::error!("actdiag: NO discovery NTF in 30s — card not coupling");
            loop {
                FreeRtos::delay_ms(5000);
            }
        }
    };

    // select with raw frame visibility
    let cmd = reader::rf_discover_select(ntf.discovery_id, ntf.protocol, ntf.interface);
    log::warn!("actdiag: SELECT cmd={:02X?}", cmd);
    let rsp = d
        .transport_mut()
        .transact(&cmd)
        .expect("select RSP timeout");
    log::warn!("actdiag: SELECT RSP: {}", show(&rsp));
    log::warn!("actdiag: SELECT status byte: {:?}", rsp.status());
    let activation = d.transport_mut().drain();
    match activation {
        Some(f) => {
            log::warn!("actdiag: ACTIVATION NTF: {}", show(&f));
            match reader::extract_ats(&f) {
                Some(ats) => log::warn!("actdiag: ATS = {:02X?}", ats),
                None => log::error!("actdiag: no ATS in activation NTF"),
            }
        }
        None => log::error!("actdiag: NO activation NTF after select"),
    }

    // and the full driver power_on path for comparison
    let mut transport = d.into_transport();
    transport.ven_cycle();
    let _ = transport.probe();
    let mut d2 = Pn7160NfcDriver::new(transport);
    d2.init().expect("ladder 2");
    let mut atr = [0u8; 64];
    match d2.power_on(&mut atr) {
        Ok(n) => log::warn!("actdiag: driver power_on OK, ATR={:02X?}", &atr[..n]),
        Err(e) => log::error!("actdiag: driver power_on FAILED: {:?}", e),
    }

    log::warn!("actdiag: done");
    loop {
        FreeRtos::delay_ms(5000);
    }
}
