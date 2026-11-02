//! pn7160-listen — ISO-DEP card-emulation main (#261's PN7160 listen
//! CE rung). Role-specific: this main NEVER runs the reader (poll)
//! ladder; the board sits in NFC-A passive listen, an external reader
//! activates it, and every C-APDU is answered ON-DEVICE from the
//! synthetic self-marked table ([`pn7160_nci::responder`]) — the
//! host-forwarded bridge is a future, separate role.
//!
//! Wire sequence and byte rationale: [`pn7160_nci::listen`] (UM11495
//! rev 1.8 §8.1.2 Table 81 / AN13861 scenario 1). Bench visibility:
//! netlog (WARN → UDP :4567) carries activation, every C-APDU/R-APDU
//! pair, and deactivation — the RF evidence channel.

use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::peripherals::Peripherals;

use pn7160_nci::listen::{self, ListenEvent, ListenSession};
use pn7160_nci::responder;

use crate::board_ctor::pn7160;
use crate::pn7160_i2c::EspPn7160Transport;

/// Mirror of the serving mains' INIT_ATTEMPTS (board_ctor): five tries
/// before declaring the frontend dead.
const LADDER_ATTEMPTS: u32 = 5;

/// Wifi association retries (bench lesson 2026-11-02: an AP outage at
/// boot left the single-shot connect network-dead for the whole session
/// while the RF path kept running — netlog/OTA evidence lost). The
/// rollback reader image re-associates via its own retry loop; this
/// main must too.
const WIFI_ATTEMPTS: u32 = 5;
const WIFI_RETRY_MS: u32 = 3000;

/// Poll cadence when no frame is pending (FreeRtos yield — a busy loop
/// starves the WiFi task and trips the task watchdog, the #63/#80/#83
/// lesson).
const POLL_IDLE_MS: u32 = 20;

fn bring_transport() -> Result<EspPn7160Transport, &'static str> {
    let peripherals = Peripherals::take().map_err(|_| "peripherals already taken")?;
    match (
        option_env!("NUCULA_WIFI_SSID"),
        option_env!("NUCULA_WIFI_PASS"),
    ) {
        (Some(ssid), Some(pass)) => {
            let nvs =
                esp_idf_svc::nvs::EspDefaultNvsPartition::take().map_err(|_| "nvs partition")?;
            match crate::wifi::WifiManager::new(peripherals.modem, nvs) {
                Ok(mut m) => {
                    let mut ip = None;
                    for attempt in 1..=WIFI_ATTEMPTS {
                        match m.connect(ssid, pass) {
                            Ok(addr) => {
                                ip = Some(addr);
                                break;
                            }
                            Err(e) => {
                                log::warn!("wifi: connect {attempt}/{WIFI_ATTEMPTS} failed: {e}");
                                FreeRtos::delay_ms(WIFI_RETRY_MS);
                            }
                        }
                    }
                    match ip {
                        Some(addr) => {
                            crate::netlog::set_ip(&addr);
                            crate::ota::spawn();
                        }
                        None => m.log_visible_aps(),
                    }
                }
                Err(e) => log::warn!("wifi: manager init failed: {e}"),
            }
        }
        _ => log::warn!("wifi: no NUCULA_WIFI_SSID/PASS baked in - serial only"),
    }

    // Same seam board_ctor::pn7160::bring_up drives for the serving
    // mains (generated pins + #447 SDA-stuck clock-out + the
    // config-order transport ctor) — everything except the READER
    // init ladder, which this role replaces with the listen ladder.
    let pins = pn7160::binding_from_board(&boards::NUCULA);
    mfrc522_pcd::recover_i2c_bus(pins.scl, pins.sda);
    let bus = pn7160::nucula_bus_pins(peripherals.i2c0, peripherals.pins);
    EspPn7160Transport::bringup_config_order(pins, bus).map_err(|_| "peripheral init")
}

/// VEN-cycle recovery between ladder attempts (the recovery hook the
/// serving path passes Pn7160NfcDriver::bring_up).
fn ladder_with_ven_retry(transport: &mut EspPn7160Transport) -> Result<(), &'static str> {
    for attempt in 1..=LADDER_ATTEMPTS {
        match listen::run_ladder_listen(transport) {
            Ok(()) => return Ok(()),
            Err(e) => {
                log::warn!("listen: ladder attempt {attempt} failed: {e} - VEN cycle");
                transport.ven_cycle();
                let _ = transport.probe();
            }
        }
    }
    Err("listen ladder exhausted")
}

pub fn run() -> ! {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();
    crate::netlog::init();
    log::warn!(
        "FWID pn7160-listen rev={} build={}",
        env!("FW_GIT_REV"),
        env!("FW_BUILD_TS")
    );

    let mut transport = match bring_transport() {
        Ok(t) => t,
        Err(e) => {
            // BusPins were consumed by the failed constructor — stay
            // loud instead of silently dead (bolty-rs B5 pattern).
            let mut dead: u32 = 0;
            loop {
                dead += 1;
                if dead % 30 == 1 {
                    log::error!("pn7160-listen: transport dead ({dead}x5s): {e}");
                }
                FreeRtos::delay_ms(5000);
            }
        }
    };

    if let Err(e) = ladder_with_ven_retry(&mut transport) {
        let mut dead: u32 = 0;
        loop {
            dead += 1;
            if dead % 30 == 1 {
                log::error!("pn7160-listen: {e} - power-cycle the board to retry");
            }
            FreeRtos::delay_ms(5000);
        }
    }
    log::warn!("listen: NFC-A passive ISO-DEP listening, routed to DH");

    let mut session = ListenSession::new();
    let mut rapdu: heapless::Vec<u8, 1024> = heapless::Vec::new();
    loop {
        match session.poll(&mut transport) {
            Some(ListenEvent::Activated {
                max_data_payload,
                initial_credits,
            }) => {
                log::warn!(
                    "listen: reader ACTIVATED (max_data={max_data_payload:#04x}, credits={initial_credits})"
                );
            }
            Some(ListenEvent::Apdu(apdu)) => {
                log::warn!("listen: C-APDU {:02X?}", apdu.as_slice());
                responder::answer(&apdu, &mut rapdu);
                log::warn!("listen: R-APDU {:02X?}", rapdu.as_slice());
                if let Err(e) = session.respond(&mut transport, &rapdu) {
                    log::warn!("listen: respond failed: {e}");
                }
            }
            Some(ListenEvent::ReaderLeft { reason }) => {
                log::warn!("listen: reader left (reason {reason:#04x}) - discovery re-arms");
            }
            Some(ListenEvent::ConnDivergence { bound, seen }) => {
                log::warn!(
                    "listen: conn divergence (bound {bound:#04x}, saw {seen:#04x}) - re-ladder"
                );
                if let Err(e) = ladder_with_ven_retry(&mut transport) {
                    log::error!("listen: recovery ladder failed: {e}");
                }
            }
            Some(ListenEvent::RxOverflow) => {
                log::warn!("listen: RX chain overflow - re-ladder");
                if let Err(e) = ladder_with_ven_retry(&mut transport) {
                    log::error!("listen: recovery ladder failed: {e}");
                }
            }
            None => FreeRtos::delay_ms(POLL_IDLE_MS),
        }
    }
}
