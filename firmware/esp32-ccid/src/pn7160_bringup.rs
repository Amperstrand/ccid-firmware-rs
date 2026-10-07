//! Post-verdict bring-up test: construct the PN7160 transport via the
//! verdict-selected variant, then run a health-check loop (issue #63)
//! that probes I2C 0x28 every 5 s. While the chip NAKs, the result is
//! logged — confirming whether it ever comes back after a power cycle.
//! First ACK triggers the NCI init ladder; success enters the card
//! heartbeat (ATR reads), failure VEN-re-cycles and keeps probing.
//! Selected by `pn7160-bringup` (implies backend-pn7160 + board-nucula);
//! the variant is one of the `pn7160-verdict-a/b/c` features. Without
//! the feature, the backend-pn7160 main runs the pad-diag probe instead.

use esp_idf_hal::delay::Ets;
use esp_idf_hal::peripherals::Peripherals;

use crate::pn7160_driver::NfcError;

use crate::nfc::NfcDriver;
use crate::pn7160_driver::Pn7160NfcDriver;
use crate::pn7160_i2c::{BusPins, EspPn7160Transport};

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

const PROBE_INTERVAL_US: u32 = 5_000_000;
const HEARTBEAT_PROBE_EVERY: u32 = 60;

// Same verdict priority as VERDICT: C alone = extended timing; A or B =
// the nci.c baseline cycle. Explicit cfg arms: a future verdict-d must
// wire itself in here, not silently inherit the extended cycle.
#[cfg(feature = "pn7160-verdict-c")]
fn ven_retrigger(t: &mut EspPn7160Transport) {
    t.ven_cycle_extended();
}
#[cfg(any(feature = "pn7160-verdict-a", feature = "pn7160-verdict-b"))]
fn ven_retrigger(t: &mut EspPn7160Transport) {
    t.ven_cycle();
}

pub fn run() -> ! {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();
    #[cfg(feature = "bench-net")]
    crate::netlog::init();
    #[cfg(not(feature = "bench-net"))]
    esp_idf_svc::log::EspLogger::initialize_default();
    log::warn!("pn7160-bringup: rust main ALIVE (verdict {})", VERDICT);
    #[cfg(not(feature = "bench-net"))]
    log::warn!("pn7160-bringup: bench-net disabled — wifi/ota/netlog excluded");

    log::warn!("step: Peripherals::take...");
    let peripherals = Peripherals::take().expect("ESP32 peripherals already taken");
    log::warn!("step: peripherals OK");

    #[cfg(feature = "bench-net")]
    match (
        option_env!("NUCULA_WIFI_SSID"),
        option_env!("NUCULA_WIFI_PASS"),
    ) {
        (Some(ssid), Some(pass)) => {
            let nvs = esp_idf_svc::nvs::EspDefaultNvsPartition::take().expect("nvs partition");
            match crate::wifi::WifiManager::new(peripherals.modem, nvs) {
                Ok(mut m) => match m.connect(ssid, pass) {
                    Ok(ip) => {
                        crate::netlog::set_ip(&ip);
                        crate::ota::spawn();
                    }
                    Err(e) => {
                        log::warn!("wifi: connect failed: {}", e);
                        m.log_visible_aps();
                    }
                },
                Err(e) => log::warn!("wifi: manager init failed: {}", e),
            }
        }
        _ => log::warn!("wifi: no NUCULA_WIFI_SSID/PASS baked in - serial only"),
    }

    for i in 1..=3u32 {
        log::warn!("bring-up starts in {}s", 4 - i);
        Ets::delay_us(1_000_000);
    }

    let bus = BusPins {
        i2c0: peripherals.i2c0,
        sda: peripherals.pins.gpio4,
        scl: peripherals.pins.gpio5,
        irq: peripherals.pins.gpio6,
        ven: peripherals.pins.gpio7,
    };

    let transport = {
        #[cfg(feature = "pn7160-verdict-a")]
        let t = EspPn7160Transport::bringup_pad_hold_clear(bus);
        #[cfg(all(feature = "pn7160-verdict-b", not(feature = "pn7160-verdict-a")))]
        let t = EspPn7160Transport::bringup_config_order(bus);
        #[cfg(all(
            feature = "pn7160-verdict-c",
            not(any(feature = "pn7160-verdict-a", feature = "pn7160-verdict-b"))
        ))]
        let t = EspPn7160Transport::bringup_ven_timing(bus);
        t
    };

    let transport = match transport {
        Ok(mut t) => {
            log::warn!("step: i2c bus scan...");
            t.i2c_scan();
            t
        }
        Err(e) => {
            log::error!("pn7160-bringup: transport bring-up FAILED: {:?}", e);
            // BusPins were consumed by the failed constructor — no driver
            // can be rebuilt, so probing is impossible. Stay loud instead
            // of silently dead (bolty-rs lessons-learned B5 pattern).
            let mut dead: u32 = 0;
            loop {
                dead += 1;
                if dead % 30 == 1 {
                    log::error!(
                        "pn7160-bringup: transport dead ({}x5s) — power-cycle the board to retry",
                        dead
                    );
                }
                Ets::delay_us(5_000_000);
            }
        }
    };

    log::warn!(
        "health: probing PN7160 @0x28 every {}s until it ACKs (issue #63)",
        PROBE_INTERVAL_US / 1_000_000
    );

    // transport and driver are mutually exclusive owners of the I2C link:
    // driver exists only between a successful init ladder and the next
    // failed health probe.
    let mut transport_slot = Some(transport);
    let mut driver: Option<Box<Pn7160NfcDriver<EspPn7160Transport>>> = None;
    let mut atr = [0u8; 64];
    let mut hb: u32 = 0;

    loop {
        if driver.is_none() {
            Ets::delay_us(PROBE_INTERVAL_US);
            hb += 1;
            let t = transport_slot
                .as_mut()
                .expect("transport must exist while driver does not");
            match t.probe() {
                Err(e) => log::warn!("health[{}]: PN7160 @0x28 no-ack: {:?}", hb, e),
                Ok(()) => {
                    log::warn!(
                        "health[{}]: PN7160 ACK @0x28 — chip is BACK, running init ladder",
                        hb
                    );
                    let t = transport_slot
                        .take()
                        .expect("transport must exist while driver does not");
                    let mut d = Box::new(Pn7160NfcDriver::new(t));
                    match d.init() {
                        Ok(()) => {
                            log::warn!("pn7160-bringup: NCI INIT LADDER OK — PN7160 ALIVE");
                            // Reset the heartbeat so the first mid-session
                            // health probe fires exactly
                            // HEARTBEAT_PROBE_EVERY seconds after session
                            // start (hb ticks at different rates across
                            // probe/heartbeat modes).
                            hb = 0;
                            driver = Some(d);
                        }
                        Err(e) => {
                            log::error!(
                                "pn7160-bringup: init ladder FAILED: {} — VEN re-cycle, keep probing",
                                nfc_err(&e)
                            );
                            let mut t = d.into_transport();
                            ven_retrigger(&mut t);
                            transport_slot = Some(t);
                        }
                    }
                }
            }
            continue;
        }

        Ets::delay_us(1_000_000);
        hb += 1;

        if hb % HEARTBEAT_PROBE_EVERY == 0 {
            let died = match driver.as_deref_mut() {
                Some(d) => match d.transport_mut().probe() {
                    Ok(()) => {
                        log::warn!("health[{}]: PN7160 still ACKing", hb);
                        false
                    }
                    Err(e) => {
                        log::error!(
                            "health[{}]: PN7160 stopped ACKing mid-session: {:?} — back to probe loop",
                            hb,
                            e
                        );
                        true
                    }
                },
                None => false,
            };
            if died {
                let d = driver.take().expect("driver in heartbeat");
                let mut t = d.into_transport();
                ven_retrigger(&mut t);
                transport_slot = Some(t);
                continue;
            }
        }

        if let Some(d) = driver.as_deref_mut() {
            if d.is_card_present() {
                match d.power_on(&mut atr) {
                    Ok(n) => log::warn!("pn7160-bringup: CARD ATR[{}]: {:02x?}", n, &atr[..n]),
                    Err(e) => log::warn!("pn7160-bringup: power_on FAILED: {}", nfc_err(&e)),
                }
                d.power_off();
            } else {
                log::warn!("pn7160-bringup: hb {} no card", hb);
            }
        }
    }
}
