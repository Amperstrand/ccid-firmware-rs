//! WiFi-CCID firmware main for the nucula C3: the GemPC serial CCID
//! server over TCP — the board's *application* transport. The native
//! USB-C port stays in its designed role (flash/console/JTAG/power,
//! issue #117: it chip-resets on host DTR/RTS by design), so console
//! logging stays ON here — the CCID-carrying wire is the TCP stream,
//! and clean-wire applies to it, not the debug console.
//!
//! Host side (proven loopback 2026-10-12): socat PTY↔TCP, stock
//! libccidtwin via reader.conf — zero custom host code.
//!
//! Feature: `wifi-ccid` (implies backend-pn7160 + board-nucula).

use esp_idf_hal::delay::TickType;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_sys::link_patches;

use crate::ccid_handler::CcidHandler;
use crate::ccid_serial_server::{CcidSerialServer, ServeAction};
use crate::nfc::NfcDriver;
use crate::pn7160_driver::Pn7160NfcDriver;
use crate::pn7160_i2c::{BusPins, EspPn7160Transport};
use crate::wifi::WifiManager;

use std::io::{Read, Write};
use std::net::TcpListener;

const CARD_POLL_INTERVAL_MS: u32 = 500;
const INIT_ATTEMPTS: u32 = 5;
const TCP_PORT: u16 = 29443;

#[link_section = ".rodata"]
static _BUILD_TAG: &[u8] = b"wifi-ccid-v1";

pub fn run() -> ! {
    link_patches();
    esp_idf_hal::sys::link_patches();

    // The CDC is the DEBUG console in this build (no CCID on it) —
    // logs stay enabled for the whole lifetime.
    use esp_idf_svc::log::EspLogger;
    EspLogger::initialize_default();

    log::warn!(
        "FWID wifi-ccid rev={} build={} transport=wifi device=\"GemPC Twin ESP32\"",
        env!("FW_GIT_REV"),
        env!("FW_BUILD_TS")
    );

    let peripherals = Peripherals::take().expect("peripherals already taken");
    let nvs = EspDefaultNvsPartition::take().expect("nvs partition");

    // Station bring-up: creds are baked by option_env! at build time
    // (build.rs rerun-if-env-changed); on failure the manager logs
    // every visible AP (the existing bring-up diagnostic).
    let mut wifi = WifiManager::new(peripherals.modem, nvs).expect("wifi init");
    let ssid = option_env!("NUCULA_WIFI_SSID").unwrap_or("");
    let pass = option_env!("NUCULA_WIFI_PASS").unwrap_or("");
    let ip = match wifi.connect(ssid, pass) {
        Ok(ip) => ip,
        Err(e) => {
            log::error!("wifi connect failed ({:?}) — serving CCID anyway is pointless without a link", e);
            wifi.log_visible_aps();
            loop {
                esp_idf_hal::delay::FreeRtos::delay_ms(10_000);
            }
        }
    };
    log::warn!("wifi-ccid: station up at {} — TCP CCID on port {}", ip, TCP_PORT);

    let listener = TcpListener::bind(("0.0.0.0", TCP_PORT)).expect("tcp bind");
    log::warn!("wifi-ccid: listening on 0.0.0.0:{}", TCP_PORT);

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
                log::warn!("wifi-ccid: PN7160 initialized (attempt {})", attempt);
                driver = Some(d);
                break;
            }
            Err(e) => {
                log::warn!("wifi-ccid: init attempt {} failed ({:?}) — VEN re-cycle + retry", attempt, e);
                let mut t = d.into_transport();
                t.ven_cycle();
                let _ = t.probe();
                transport = Some(t);
            }
        }
    }
    let driver = driver.unwrap_or_else(|| {
        log::warn!("wifi-ccid: PN7160 unresponsive after {} attempts — card-absent mode", INIT_ATTEMPTS);
        Pn7160NfcDriver::new(transport.take().expect("transport"))
    });

    let poll_interval = TickType::new_millis(CARD_POLL_INTERVAL_MS as u64).ticks() as u32;
    let mut server = CcidSerialServer::new(CcidHandler::new(driver), poll_interval, unsafe {
        esp_idf_sys::xTaskGetTickCount()
    });

    // One client at a time (pcscd is the only intended consumer); a
    // dropped client returns to accept. Byte contract identical to the
    // CDC/SPP paths: feed per byte, echo + response on completion.
    loop {
        let (mut stream, peer) = match listener.accept() {
            Ok(s) => s,
            Err(e) => {
                log::warn!("wifi-ccid: accept error ({:?}) — retrying", e);
                esp_idf_hal::delay::FreeRtos::delay_ms(500);
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        log::warn!("wifi-ccid: client {} connected", peer);
        let mut burst_buf = [0u8; 64];
        loop {
            match stream.read(&mut burst_buf) {
                Ok(0) => {
                    log::warn!("wifi-ccid: client {} closed", peer);
                    break;
                }
                Ok(n) => {
                    for &byte in &burst_buf[..n] {
                        let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
                        if server.feed_byte(byte, now) == ServeAction::Respond {
                            let echo = server.echo();
                            let resp = server.response();
                            if stream.write_all(echo).is_err() || stream.write_all(resp).is_err() {
                                break;
                            }
                            // Dump-and-retrieve: escape 0xD1 ack went out —
                            // panic now so the flash coredump captures state.
                            if server.take_snapshot_request() {
                                panic!("escape 0xD1: diagnostic snapshot requested");
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("wifi-ccid: read error ({:?}) — dropping client", e);
                    break;
                }
            }
            let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
            server.poll_if_due(now);
        }
    }
}
