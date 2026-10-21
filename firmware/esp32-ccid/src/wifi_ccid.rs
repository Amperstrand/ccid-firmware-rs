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
use crate::ccid_serial_server::{CcidSerialServer, ResponseWindows};
use crate::nfc::NfcDriver;
use crate::pn7160_driver::Pn7160NfcDriver;
use crate::pn7160_i2c::{BusPins, EspPn7160Transport};
use crate::wifi::WifiManager;

use std::io::{ErrorKind, Write};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicI32, Ordering};

const CARD_POLL_INTERVAL_MS: u32 = 500;
const INIT_ATTEMPTS: u32 = 5;
const TCP_PORT: u16 = 29443;
/// Consecutive NAKed I2C writes that mean the PN7160 wedged (mute):
/// below this the driver's retry cadence absorbs transient bus errors.
const I2C_MUTE_THRESHOLD: u32 = 3;

#[link_section = ".rodata"]
static _BUILD_TAG: &[u8] = b"wifi-ccid-v1";

/// Connected session's TCP fd (-1 = no session) — the stall-hook
/// writer's only reach into the accept loop's stack-local stream.
static SESSION_FD: AtomicI32 = AtomicI32::new(-1);

/// Raw TCP writer for the time-extension stall hook (issue #156 B):
/// runs inside the blocking card exchange, where the serve loop's
/// `stream` reference is unreachable. Borrows the live fd without
/// consuming it (`forget` keeps the owning stream valid).
fn write_wire_tcp(bytes: &[u8]) {
    use std::os::fd::FromRawFd;
    let fd = SESSION_FD.load(Ordering::SeqCst);
    if fd < 0 {
        return;
    }
    // SAFETY: the fd is owned by the accept loop's live `TcpStream`
    // (published via SESSION_FD). The borrowed handle is forgotten
    // before drop could close the fd — the owning stream stays valid.
    let mut borrowed = unsafe { std::net::TcpStream::from_raw_fd(fd) };
    let _ = borrowed.write_all(bytes);
    std::mem::forget(borrowed);
}

pub fn run() -> ! {
    link_patches();
    esp_idf_hal::sys::link_patches();
    crate::install_platform_clock();

    // netlog writes every log line to the serial console AND (once
    // the network is up) to a UDP broadcast on :4567 — the bench log
    // channel when no USB link exists (wall-power operation). Replaces
    // EspLogger (first installer wins in the log crate).
    crate::netlog::init();

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
            log::error!(
                "wifi connect failed ({:?}) — serving CCID anyway is pointless without a link",
                e
            );
            wifi.log_visible_aps();
            loop {
                esp_idf_hal::delay::FreeRtos::delay_ms(10_000);
            }
        }
    };
    log::warn!(
        "wifi-ccid: station up at {} — TCP CCID on port {}",
        ip,
        TCP_PORT
    );

    // WiFi debug + OTA (bench-net builds only, #219): bind the netlog
    // UDP socket now that lwIP is up (binding earlier asserts); spawn
    // the TCP OTA flash server (:3232) so bench boards can be updated
    // without a USB link. Shipped builds compile neither surface.
    #[cfg(feature = "bench-net")]
    crate::netlog::set_ip(&ip);
    #[cfg(feature = "bench-net")]
    crate::ota::spawn();

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
    let transport =
        EspPn7160Transport::bringup_config_order(pn7160_nci::esp_transport::NUCULA, bus)
            .expect("peripheral init");
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
                log::warn!(
                    "wifi-ccid: init attempt {} failed ({:?}) — VEN re-cycle + retry",
                    attempt,
                    e
                );
                let mut t = d.into_transport();
                t.ven_cycle();
                let _ = t.probe();
                transport = Some(t);
            }
        }
    }
    let mut driver = driver.unwrap_or_else(|| {
        log::warn!(
            "wifi-ccid: PN7160 unresponsive after {} attempts — card-absent mode",
            INIT_ATTEMPTS
        );
        Pn7160NfcDriver::new(transport.take().expect("transport"))
    });
    // Issue #156 B: interim CCID time-extension frames during a
    // stalling (WTX) exchange — hook + raw writer registration.
    crate::ccid_serial_server::time_extension::register_writer(write_wire_tcp);
    driver.set_stall_hook(Some(crate::ccid_serial_server::time_extension::stall_hook));

    let poll_interval = TickType::new_millis(CARD_POLL_INTERVAL_MS as u64).ticks() as u32;
    let response_windows = ResponseWindows::from_tick_hz(esp_idf_sys::configTICK_RATE_HZ);
    let mut server = CcidSerialServer::new(
        CcidHandler::new(driver),
        poll_interval,
        response_windows,
        unsafe { esp_idf_sys::xTaskGetTickCount() },
    );

    // Single-session serve loop, tick architecture (bench hardening
    // 2026-10-12): the session loop lives in the serving kernel
    // (`ccid_service::tcp_server`, t_72e1e2ed) — host-tested there —
    // with the #153 arrival-floor + idle-close semantics carried over
    // byte-identically. This main keeps the board half: session-fd
    // publication for the stall hook, the accept/reject outer loop,
    // and the mute-recovery ladder injected as the kernel's on-tick
    // hook (the kernel must not reach into pn7160_i2c).
    listener
        .set_nonblocking(true)
        .expect("listener nonblocking");

    loop {
        let (mut stream, peer) = loop {
            match listener.accept() {
                Ok(pair) => break pair,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    esp_idf_hal::delay::FreeRtos::delay_ms(50);
                }
                Err(e) => {
                    log::warn!("wifi-ccid: accept error ({:?}) — retrying", e);
                    esp_idf_hal::delay::FreeRtos::delay_ms(500);
                }
            }
        };
        let _ = stream.set_nodelay(true);
        SESSION_FD.store(stream.as_raw_fd(), Ordering::SeqCst);
        log::warn!("wifi-ccid: client {} connected", peer);
        // Mute recovery (two faces, bench 2026-10-12): a NAKing PN7160
        // (send streak) or a semi-muted one that ACKs writes but never
        // answers RSPs (silent streak) — both stay wedged until VEN
        // power cycle; power_off clears driver state, the ladder
        // re-inits. Injected at the kernel boundary: the kernel calls
        // this once per serve tick with the live driver.
        let mut mute_recovery = |drv: &mut Pn7160NfcDriver<EspPn7160Transport>| {
            let send_streak = crate::pn7160_i2c::i2c_send_failure_streak();
            let silent_streak = u32::from(drv.silent_rearm_streak());
            if send_streak >= I2C_MUTE_THRESHOLD || silent_streak >= I2C_MUTE_THRESHOLD {
                log::warn!(
                    "wifi-ccid: PN7160 mute (send={}, silent={}) — VEN cycle + ladder replay",
                    send_streak,
                    silent_streak
                );
                drv.power_off();
                drv.transport_mut().ven_cycle();
                match drv.init() {
                    Ok(()) => log::warn!("wifi-ccid: PN7160 recovered (ladder ok)"),
                    Err(e) => log::warn!("wifi-ccid: recovery ladder failed ({:?})", e),
                }
            }
        };
        let mut clock = || unsafe { esp_idf_sys::xTaskGetTickCount() };
        let _session_end = ccid_service::tcp_server::serve_tcp_session(
            &mut stream,
            &listener,
            &mut server,
            &ccid_service::tcp_server::SessionParams {
                cfg: ccid_service::tcp_server::SessionConfig::default(),
                log_tag: "wifi-ccid",
                peer,
            },
            &mut clock,
            &mut mute_recovery,
        );
        // Session over (closed/idle/error): retire the stall-hook fd
        // before the stream drops and the fd could be reused.
        SESSION_FD.store(-1, Ordering::SeqCst);
    }
}
