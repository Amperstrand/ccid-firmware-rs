//! Shared WiFi bring-up for the nucula's network firmware paths
//! (wifi-ccid legacy and wifi-vpcd vsmartcard-aligned).
//! Extracted so both paths get identical WiFi, PN7160, netlog, and OTA
//! setup without duplication.

use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;

use crate::nfc::NfcDriver;
use crate::pn7160_driver::Pn7160NfcDriver;
use crate::pn7160_i2c::{BusPins, EspPn7160Transport};
use crate::wifi::WifiManager;

const INIT_ATTEMPTS: u32 = 5;

/// Connect WiFi, bring up PN7160, start netlog and OTA.
/// Returns the IP string and the NFC driver, ready to serve.
pub fn bring_up() -> (String, Pn7160NfcDriver<EspPn7160Transport>) {
    let peripherals = Peripherals::take().expect("peripherals already taken");
    let nvs = EspDefaultNvsPartition::take().expect("nvs partition");

    let mut wifi = WifiManager::new(peripherals.modem, nvs).expect("wifi init");
    let ssid = option_env!("NUCULA_WIFI_SSID").unwrap_or("");
    let pass = option_env!("NUCULA_WIFI_PASS").unwrap_or("");
    let ip = wifi.connect(ssid, pass).unwrap_or_else(|e| {
        log::error!("wifi connect failed ({:?})", e);
        wifi.log_visible_aps();
        "0.0.0.0".to_string()
    });
    // The WifiManager owns the station — dropping it tears down the
    // WiFi link. Leak it so the connection outlives bring_up().
    std::mem::forget(wifi);

    crate::netlog::set_ip(&ip);
    crate::ota::spawn();

    // OTA_ROLLBACK: signal the bootloader that this image boots and serves
    // (esp-idf OTA Update Pipeline: the bootloader boots a new OTA slot in
    // a "pending verify" state; without this call it rolls back to the
    // previous slot on the next boot, which is exactly the dual-boot
    // recovery we want — a bad flash auto-recovers to the last good image).
    unsafe {
        let rc = esp_idf_sys::esp_ota_mark_app_valid_cancel_rollback();
        if rc != 0 {
            log::warn!("ota-rollback: mark_app_valid rc={} (not in pending state — first boot or factory)", rc);
        }
    }

    // PN7160 bring-up — the ACK-window ladder, verbatim from the CDC main.
    let bus = BusPins {
        i2c0: peripherals.i2c0,
        sda: peripherals.pins.gpio4,
        scl: peripherals.pins.gpio5,
        irq: peripherals.pins.gpio6,
        ven: peripherals.pins.gpio7,
    };
    let transport = EspPn7160Transport::bringup_config_order(bus).expect("peripheral init");
    let mut transport = Some(transport);
    let mut driver: Option<Pn7160NfcDriver<EspPn7160Transport>> = None;
    for attempt in 1..=INIT_ATTEMPTS {
        let mut d = Pn7160NfcDriver::new(transport.take().expect("transport"));
        match d.init() {
            Ok(()) => {
                log::warn!("bring-up: PN7160 initialized (attempt {})", attempt);
                driver = Some(d);
                break;
            }
            Err(e) => {
                log::warn!("bring-up: init attempt {} failed ({:?})", attempt, e);
                let mut t = d.into_transport();
                t.ven_cycle();
                let _ = t.probe();
                transport = Some(t);
            }
        }
    }
    let driver = driver.unwrap_or_else(|| {
        log::warn!("bring-up: PN7160 unresponsive — card-absent mode");
        Pn7160NfcDriver::new(transport.take().expect("transport"))
    });

    (ip, driver)
}
