//! Post-verdict bring-up test: construct the PN7160 transport via the
//! verdict-selected variant, run the NCI init ladder, then heartbeat
//! card presence with ATR reads. Selected by `pn7160-bringup` (implies
//! backend-pn7160 + board-nucula); the variant is one of the
//! `pn7160-verdict-a/b/c` features. Without the feature, the
//! backend-pn7160 main runs the pad-diag probe instead.

use esp_idf_hal::delay::Ets;
use esp_idf_hal::peripherals::Peripherals;

use crate::pn7160_driver::NfcError;

use crate::nfc::NfcDriver;
use crate::pn7160_driver::Pn7160NfcDriver;
use crate::pn7160_i2c::EspPn7160Transport;

#[cfg(feature = "pn7160-verdict-a")]
const VERDICT: &str = "A/pad-hold-clear";
#[cfg(all(feature = "pn7160-verdict-b", not(feature = "pn7160-verdict-a")))]
const VERDICT: &str = "B/config-order";
#[cfg(all(
    feature = "pn7160-verdict-c",
    not(any(feature = "pn7160-verdict-a", feature = "pn7160-verdict-b"))
))]
const VERDICT: &str = "C/ven-timing";
#[cfg(not(any(
    feature = "pn7160-verdict-a",
    feature = "pn7160-verdict-b",
    feature = "pn7160-verdict-c"
)))]
compile_error!("pn7160-bringup needs one of pn7160-verdict-a/b/c");

fn nfc_err(e: &NfcError) -> &'static str {
    match e {
        NfcError::BringUp(_) => "BringUp",
        NfcError::Select(_) => "Select",
        NfcError::NoCard => "NoCard",
        NfcError::ExchangeFailed => "ExchangeFailed",
        NfcError::BufferTooSmall => "BufferTooSmall",
    }
}

pub fn run() -> ! {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::set_max_level(log::LevelFilter::Debug);
    log::warn!("pn7160-bringup: rust main ALIVE (verdict {})", VERDICT);

    for i in 1..=3u32 {
        log::warn!("bring-up starts in {}s", 4 - i);
        Ets::delay_us(1_000_000);
    }
    log::warn!("step: Peripherals::take...");
    let peripherals = Peripherals::take().expect("ESP32 peripherals already taken");
    log::warn!("step: peripherals OK");

    let transport = {
        #[cfg(feature = "pn7160-verdict-a")]
        let t = EspPn7160Transport::bringup_pad_hold_clear(peripherals);
        #[cfg(all(feature = "pn7160-verdict-b", not(feature = "pn7160-verdict-a")))]
        let t = EspPn7160Transport::bringup_config_order(peripherals);
        #[cfg(all(
            feature = "pn7160-verdict-c",
            not(any(feature = "pn7160-verdict-a", feature = "pn7160-verdict-b"))
        ))]
        let t = EspPn7160Transport::bringup_ven_timing(peripherals);
        t
    };

    let mut driver = match transport {
        Ok(mut t) => {
            log::warn!("step: i2c bus scan...");
            t.i2c_scan();
            Box::new(Pn7160NfcDriver::new(t))
        }
        Err(e) => {
            log::error!("pn7160-bringup: transport bring-up FAILED: {:?}", e);
            loop {
                Ets::delay_us(1_000_000);
            }
        }
    };

    match driver.init() {
        Ok(()) => log::warn!("pn7160-bringup: NCI INIT LADDER OK — PN7160 ALIVE"),
        Err(e) => log::error!("pn7160-bringup: init ladder FAILED: {}", nfc_err(&e)),
    }

    let mut atr = [0u8; 64];
    let mut tick: u32 = 0;
    loop {
        Ets::delay_us(1_000_000);
        tick += 1;
        if driver.is_card_present() {
            match driver.power_on(&mut atr) {
                Ok(n) => log::warn!("pn7160-bringup: CARD ATR[{}]: {:02x?}", n, &atr[..n]),
                Err(e) => log::warn!("pn7160-bringup: power_on FAILED: {}", nfc_err(&e)),
            }
            driver.power_off();
        } else {
            log::warn!("pn7160-bringup: hb {} no card", tick);
        }
    }
}
