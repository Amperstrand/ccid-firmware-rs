//! ROOT CAUSE FIX (issues #63/#80/#83): all delays >= 1ms MUST use
//! FreeRtos::delay_ms (yields to scheduler), NEVER Ets::delay_us (busy
//! wait). Busy-waiting starves the IDLE task → task watchdog fires →
//! register-dump floods the console → interrupt storm corrupts the I2C
//! ISR's ability to handle the PN7160's clock stretching → permanent NAK.
//! The PCF8574 keyboard (no clock stretching) is unaffected.
//!
//! Post-verdict bring-up test: construct the PN7160 transport via the
//! verdict-selected variant, then run a health-check loop (issue #63)
//! that probes I2C 0x28 every 5 s. While the chip NAKs, the result is
//! logged — confirming whether it ever comes back after a power cycle.
//! First ACK triggers the NCI init ladder; success enters the card
//! heartbeat (ATR reads), failure VEN-re-cycles and keeps probing.
//! Selected by `pn7160-bringup` (implies backend-pn7160 + board-nucula);
//! the variant is one of the `pn7160-verdict-a/b/c` features. Without
//! the feature, the backend-pn7160 main runs the pad-diag probe instead.

use esp_idf_hal::delay::{Ets, FreeRtos};
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
    log::warn!("FWID pn7160-bringup rev={} build={}", env!("FW_GIT_REV"), env!("FW_BUILD_TS"));
    log::warn!("pn7160-bringup: rust main ALIVE (verdict {})", VERDICT);

    log::warn!("step: Peripherals::take...");
    let peripherals = Peripherals::take().expect("ESP32 peripherals already taken");
    log::warn!("step: peripherals OK");

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
        Ok(t) => {
            // K-test: SCAN SKIPPED — probe 0x28 directly like the wallet fw
            // (nci.c probes without ever scanning). If the 127-probe scan
            // poisons driver/bus state, direct-probe now succeeds.
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
                FreeRtos::delay_ms(5000);
            }
        }
    };

    log::warn!(
        "health: probing PN7160 @0x28 every {}s until it ACKs (issue #63) [FWID re-issue rev={}]",
        PROBE_INTERVAL_US / 1_000_000,
        env!("FW_GIT_REV")
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
            FreeRtos::delay_ms(5000);
            hb += 1;
            let mut recovered = false;
            {
                let t = transport_slot
                    .as_mut()
                    .expect("transport must exist while driver does not");
            match t.probe() {
                Err(e) => {
                    log::warn!(
                        "health[{}]: PN7160 @0x28 no-ack: {:?} (isr_rc={}/{})*",
                        hb,
                        e,
                        crate::pn7160_i2c::ISR_SERVICE_RC.load(core::sync::atomic::Ordering::Relaxed),
                        crate::pn7160_i2c::ISR_ADD_RC.load(core::sync::atomic::Ordering::Relaxed)
                    );
                    // ROOT CAUSE (#63/#80, bench-proven 2026-10-08 via C
                    // controls v1-v10): the PN7160's I2C slave only ACKs
                    // within a window after VEN rise. Probing 5s later is
                    // forever mute; the wallet ACKs because nci_init
                    // probes 50ms after the VEN cycle. Fix: re-cycle VEN
                    // and probe IMMEDIATELY after it.
                    log::warn!("health[{}]: VEN re-cycle + immediate probe", hb);
                    t.ven_cycle();
                    match t.probe() {
                        Ok(()) => {
                            log::warn!(
                                "health[{}]: PN7160 ACK @0x28 after VEN re-cycle — running init ladder",
                                hb
                            );
                            recovered = true;
                        }
                        Err(e2) => {
                            log::warn!("health[{}]: still no-ack after re-cycle: {:?}", hb, e2);
                        }
                    }
                    // nucula-board wiring-audit: ADR0/ADR1 are strapped via
                    // 100k pull-downs against the PN7160's internal 55-120k
                    // pull-ups — strap level 0.46-0.65 x VDD is INDETERMINATE
                    // (guaranteed LOW needs <= 0.35). The shipped R2 BOM kept
                    // 100k (prescribed fix was 0R/10k). If the chip samples
                    // the straps high at VEN rise it lands on 0x29-0x2B.
                    if hb % 6 == 1 {
                        for alt in [0x29u8, 0x2A, 0x2B] {
                            if let Ok(()) = t.probe_addr(alt) {
                                log::warn!(
                                    "health[{}]: *** PN7160 responds at 0x{:02X} — strap margin CONFIRMED (fix: R23/R24 -> 0R)",
                                    hb, alt
                                );
                            }
                        }
                    }
                }
                Ok(()) => {
                    log::warn!(
                        "health[{}]: PN7160 ACK @0x28 — chip is BACK, running init ladder",
                        hb
                    );
                    recovered = true;
                }
            }
            }
            if recovered {
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
            continue;
        }

        FreeRtos::delay_ms(1000);
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
