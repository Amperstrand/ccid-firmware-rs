//! Shared bring-up for the network firmware paths (wifi-ccid legacy,
//! wifi-vpcd primary). `station_bring_up` is backend-agnostic (WiFi,
//! netlog, OTA, rollback mark); each NFC backend gets its own ladder so
//! `wifi-vpcd` composes with any board/backend feature pair:
//!
//!   nucula:  `--features wifi-vpcd,backend-pn7160,board-nucula`
//!   m5atom:  `--features wifi-vpcd,backend-mfrc522,board-m5atom`
//!   m5stick: `--features wifi-vpcd,backend-mfrc522,board-m5stick`

use esp_idf_svc::nvs::EspDefaultNvsPartition;

#[cfg(feature = "backend-pn7160")]
use crate::nfc::NfcDriver;
#[cfg(feature = "backend-pn7160")]
use crate::pn7160_driver::Pn7160NfcDriver;
#[cfg(feature = "backend-pn7160")]
use crate::pn7160_i2c::{BusPins, EspPn7160Transport};
#[cfg(feature = "backend-pn7160")]
use esp_idf_hal::peripherals::Peripherals;

const INIT_ATTEMPTS: u32 = 5;

/// WiFi station + (bench-net only) netlog/OTA + rollback mark-valid.
/// Returns the station IP (never fails — on connect failure the board
/// runs with "0.0.0.0" and keeps broadcasting its state on netlog).
pub fn station_bring_up(
    modem: esp_idf_hal::modem::Modem<'static>,
    nvs: EspDefaultNvsPartition,
) -> String {
    use crate::wifi::WifiManager;

    let mut wifi = WifiManager::new(modem, nvs).expect("wifi init");
    let ssid = option_env!("NUCULA_WIFI_SSID").unwrap_or("");
    let pass = option_env!("NUCULA_WIFI_PASS").unwrap_or("");
    let ip = wifi.connect(ssid, pass).unwrap_or_else(|e| {
        log::error!("wifi connect failed ({:?})", e);
        wifi.log_visible_aps();
        "0.0.0.0".to_string()
    });
    // The WifiManager owns the station — dropping it tears down the
    // WiFi link. Leak it so the connection outlives this call.
    std::mem::forget(wifi);

    // bench-net only (#219): shipped transports compile no OTA listener
    // and no netlog UDP arm — the bench builds opt in via build.sh.
    #[cfg(feature = "bench-net")]
    crate::netlog::set_ip(&ip);
    #[cfg(feature = "bench-net")]
    crate::ota::spawn();

    // OTA_ROLLBACK: signal the bootloader that this image boots and serves
    // (esp-idf OTA Update Pipeline: the bootloader boots a new OTA slot in
    // a "pending verify" state; without this call it rolls back to the
    // previous slot on the next boot — a bad flash auto-recovers to the
    // last good image).
    unsafe {
        let rc = esp_idf_sys::esp_ota_mark_app_valid_cancel_rollback();
        if rc != 0 {
            log::warn!(
                "ota-rollback: mark_app_valid rc={} (not in pending state — first boot or factory)",
                rc
            );
        }
    }

    ip
}

/// Convenience: full bring-up including `Peripherals::take()` when only
/// the PN7160/nucula composition is needed (the legacy wifi-ccid shape).
#[cfg(feature = "backend-pn7160")]
pub fn bring_up() -> (String, Pn7160NfcDriver<EspPn7160Transport>) {
    let peripherals = Peripherals::take().expect("peripherals already taken");
    let nvs = EspDefaultNvsPartition::take().expect("nvs partition");
    let ip = station_bring_up(peripherals.modem, nvs);
    let bus = BusPins {
        i2c0: peripherals.i2c0,
        sda: peripherals.pins.gpio4,
        scl: peripherals.pins.gpio5,
        irq: peripherals.pins.gpio6,
        ven: peripherals.pins.gpio7,
    };
    (ip, pn7160_bring_up(bus))
}

/// PN7160 ladder (nucula): the ACK-window bring-up with VEN-cycle
/// retries, verbatim from the CDC main.
#[cfg(feature = "backend-pn7160")]
pub fn pn7160_bring_up<I2C, SDA, SCL, IRQ, VEN>(
    bus: BusPins<I2C, SDA, SCL, IRQ, VEN>,
) -> Pn7160NfcDriver<EspPn7160Transport> {
    let transport =
        EspPn7160Transport::bringup_config_order(pn7160_nci::esp_transport::NUCULA, bus)
            .expect("peripheral init");
    // t_4864e74e: the VEN-cycle retry ladder is the driver's own
    // constructor now — this wrapper only supplies the esp recovery
    // hook (VEN cycle + bus probe between attempts).
    let (driver, _ok) = Pn7160NfcDriver::bring_up(transport, INIT_ATTEMPTS, |t| {
        t.ven_cycle();
        let _ = t.probe();
    });
    driver
}

/// Reader-present or card-absent stand-in. When the NFC frontend never
/// answers at bring-up, `OrNull::Null` keeps the vpcd link, OTA server
/// and netlog alive: pcscd sees a permanently EMPTY reader instead of
/// the board vanishing — the bench can tell "reader broken" (reader
/// online, no card) from "board gone" (no connection at all).
///
/// The Null arm's Ok(0) answers are unreachable in practice: every
/// caller gates on `is_card_present()` first, and Null reports false.
pub enum OrNull<D> {
    Live(D),
    Null,
}

impl<D: crate::nfc::NfcDriver> crate::nfc::NfcDriver for OrNull<D> {
    type Error = D::Error;

    fn init(&mut self) -> Result<(), Self::Error> {
        match self {
            OrNull::Live(d) => d.init(),
            OrNull::Null => Ok(()),
        }
    }

    fn is_card_present(&mut self) -> bool {
        match self {
            OrNull::Live(d) => d.is_card_present(),
            OrNull::Null => false,
        }
    }

    fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
        match self {
            OrNull::Live(d) => d.power_on(atr_buf),
            OrNull::Null => Ok(0),
        }
    }

    fn power_off(&mut self) {
        if let OrNull::Live(d) = self {
            d.power_off();
        }
    }

    fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Self::Error> {
        match self {
            OrNull::Live(d) => d.transmit_apdu(command, response),
            OrNull::Null => Ok(0),
        }
    }
}

impl<D: crate::nfc::NfcDriver + card_interface::FrontendDiagnostics>
    card_interface::FrontendDiagnostics for OrNull<D>
{
    fn frontend_status(&self) -> card_interface::FrontendStatus {
        match self {
            // t_4864e74e: the Null arm IS the unavailable class — the
            // controller never answered at bring-up.
            OrNull::Live(d) => d.frontend_status(),
            OrNull::Null => card_interface::FrontendStatus {
                health: card_interface::FrontendHealth::Unavailable,
                last_error: card_interface::FrontendErrorKind::Bus,
                ..card_interface::FrontendStatus::ok()
            },
        }
    }

    fn frontend_self_test(&mut self) -> Result<(), card_interface::FrontendErrorKind> {
        match self {
            OrNull::Live(d) => d.frontend_self_test(),
            OrNull::Null => Err(card_interface::FrontendErrorKind::Bus),
        }
    }
}

/// MFRC522 ladder (m5atom/m5stick, Grove I2C @0x28): bus recovery,
/// 400 kHz I2C, then `init_with_recovery` retries — the stock m5stick
/// CDC main sequence, minus the UART/LED CCID plumbing. Bus + device
/// ride the #149 P3 shared driver_ng adapter (`pn7160_nci::hal_i2c`),
/// so raw GPIO numbers are the only board input.
#[cfg(feature = "backend-mfrc522")]
pub fn mfrc522_bring_up(
    scl_gpio_no: i32,
    sda_gpio_no: i32,
) -> impl crate::nfc::NfcDriver + Send + 'static {
    // I2C bus recovery (xtensa only — the riscv32 crash-probe path skips
    // the settle delay, mirroring the CDC main).
    #[cfg(target_arch = "xtensa")]
    {
        mfrc522_pcd::recover_i2c_bus(scl_gpio_no, sda_gpio_no);
        esp_idf_hal::delay::FreeRtos::delay_ms(50);
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        let _ = (scl_gpio_no, sda_gpio_no);
        log::warn!("bring-up: skipping vTaskDelay on riscv32 (crash probe)");
    }

    // 400 kHz (was 100 kHz): re-attempted per #59 with a measured RTT
    // table and a 100-transaction soak before landing.
    log::warn!("bring-up: driver_ng i2c_new_master_bus");
    let mut i2c_bus = pn7160_nci::hal_i2c::I2cMasterBus::new(0, sda_gpio_no, scl_gpio_no)
        .expect("I2C0 init failed");
    log::warn!("bring-up: I2C0 ok (driver_ng)");

    let probe_found = i2c_bus.probe(0x28, 100).is_ok();
    log::info!(
        "i2c probe @0x28: {}",
        if probe_found { "ack" } else { "no-ack" }
    );

    let i2c_dev = match i2c_bus.add_hal_device(0x28, 400_000) {
        Ok(dev) => dev,
        Err(e) => {
            log::error!(
                "MFRC522 device add failed ({e:?}) — serving as empty reader (OrNull::Null)"
            );
            return OrNull::Null;
        }
    };

    // Reader frontend absent or electrically stuck (bench 2026-10-13: SDA
    // held LOW past 32 recovery clocks on BOTH Grove pin maps). Degrade to
    // the null driver instead of halting: the vpcd link comes up as an
    // empty reader and the board stays OTA-flasheable + netlog-visible,
    // so the bench distinguishes "reader broken" from "board gone".
    let hw = match mfrc522::Mfrc522::new(mfrc522::comm::blocking::i2c::I2cInterface::new(
        i2c_dev, 0x28,
    ))
    .init()
    {
        Ok(hw) => hw,
        Err(e) => {
            log::error!("MFRC522 init failed ({e:?}) — serving as empty reader (OrNull::Null)");
            return OrNull::Null;
        }
    };

    let transceiver = mfrc522_pcd::Mfrc522Transceiver::new(hw);
    let mut driver = crate::mfrc522_driver::Mfrc522NfcDriver::new(transceiver);

    let init_ok = (0..INIT_ATTEMPTS).any(|_| {
        if driver.init_with_recovery().is_ok() {
            true
        } else {
            esp_idf_hal::delay::FreeRtos::delay_ms(1000);
            false
        }
    });
    log::warn!(
        "bring-up: MFRC522 {}",
        if init_ok {
            "initialized"
        } else {
            "init retries exhausted — card-absent mode"
        }
    );

    OrNull::Live(driver)
}
