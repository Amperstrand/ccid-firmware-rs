//! CCID firmware main for the nucula C3: runs the full CCID handler
//! over the USB-CDC serial port. The PN7160 init uses the ACK-window
//! rule (VEN re-cycle + immediate probe; see AGENTS.md) with bounded
//! retries — if it still fails, the CCID layer responds to host queries
//! with "card absent", giving a testable CCID endpoint.
//!
//! Feature: `pn7160-ccid` (implies backend-pn7160 + board-nucula).

use esp_idf_hal::delay::TickType;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::usb_serial::{config::Config as UsbConfig, UsbSerialDriver};

use esp_idf_sys::link_patches;

use crate::ccid_handler::CcidHandler;
use crate::ccid_serial_server::{CcidSerialServer, ResponseWindows, ServeAction};
use crate::nfc::NfcDriver;
use crate::pn7160_driver::Pn7160NfcDriver;
use crate::pn7160_i2c::{BusPins, EspPn7160Transport};

const UART_RX_TIMEOUT_MS: u32 = 100;
const CARD_POLL_INTERVAL_MS: u32 = 500;
const INIT_ATTEMPTS: u32 = 5;

#[link_section = ".rodata"]
static _BUILD_TAG: &[u8] = b"pn7160-ccid-v3";

/// Raw CDC writer for the time-extension stall hook (issue #156 B):
/// the hook runs inside the blocking card exchange where the serve
/// loop's `usb` driver reference is unreachable — the ESP-IDF C call
/// writes the same peripheral the UsbSerialDriver serves (the driver
/// has no Rust-side state for a blocking write).
fn write_wire_cdc(bytes: &[u8]) {
    unsafe {
        esp_idf_sys::usb_serial_jtag_write_bytes(
            bytes.as_ptr() as *const core::ffi::c_void,
            bytes.len(),
            100, // ms ticks (1000 Hz tick rate)
        );
    }
}

pub fn run() -> ! {
    link_patches();
    esp_idf_hal::sys::link_patches();
    // Silence C-level ESP_LOGx IMMEDIATELY (before any PN7160 I2C
    // activity): the driver's persistent-retry NACK messages from
    // ESP-IDF's i2c.master bypass the Rust `log` crate and pollute
    // the CCID wire before the serve loop's own Off ceiling takes
    // effect. pcscd's serial handshake reads them as garbage and the
    // reader never enumerates. The FWID below does NOT print either:
    // this NONE silencing precedes EspLogger init, so the reader image
    // is console-silent from app_main() (bench-verified 2026-10-17,
    // #149 bench230) — identity is proven by the baked FWID build-tag
    // string + serving behavior, never the console banner.
    #[cfg(not(feature = "ble"))]
    {
        use esp_idf_sys::{esp_log_level_set, esp_log_level_t_ESP_LOG_NONE};
        unsafe { esp_log_level_set(b"*\0".as_ptr().cast(), esp_log_level_t_ESP_LOG_NONE) };
    }

    // Clean-wire (the #91 shim is RETIRED, issue #131): EspLogger is
    // initialized for the banner SLOT below, but the C-level NONE
    // silencing above runs first — on this main the banner line is
    // itself console-silent (bench 2026-10-17, #149); the OFF ceiling
    // still lands right after the slot, BEFORE the USB-CDC driver
    // claim, so nothing unsolicited is ever written to the CCID wire.
    // Never call EspLogger::initialize_default() under `ble`
    // (BleConsole installs the global sink there — first installer
    // wins).
    #[cfg(not(feature = "ble"))]
    {
        use esp_idf_svc::log::EspLogger;
        EspLogger::initialize_default();
    }

    let peripherals = Peripherals::take().expect("peripherals already taken");

    // BLE debug console (issue #66): firmware logs ride NUS GATT
    // notifications so the USB-CDC port carries CCID frames only.
    // Installs the global `log` sink (no EspLogger under `ble` — first
    // installer wins) and silences C-level ESP_LOGx on the shared CDC.
    // The FWID banner below lands in the BLE ring and greets the first
    // central to attach; it no longer appears on the console in ble
    // builds (verify those via the capture tool or a non-ble build).
    #[cfg(feature = "ble")]
    // `ble-diag` (#165): keep C-level BT stack logs on the console.
    let ble_console =
        crate::ble_console::BleConsole::init(peripherals.modem, !cfg!(feature = "ble-diag"));

    log::warn!(
        "FWID pn7160-ccid rev={} build={}",
        env!("FW_GIT_REV"),
        env!("FW_BUILD_TS")
    );
    log::warn!("pn7160-ccid: rust main ALIVE");

    // Universal clean-wire ceiling (issue #131): the banner above is
    // the LAST unsolicited output — everything after this point (driver
    // claim, PN7160 bring-up, serve loop) is silent on the CCID wire by
    // construction. C-level ESP_LOGx was already silenced at the top of
    // main; this gates the Rust `log` facade. Diagnostics live in
    // Escape 0xD0 counters, 0xD1 coredumps, and the ble/actdiag builds.
    #[cfg(not(feature = "ble"))]
    log::set_max_level(log::LevelFilter::Off);

    // USB-CDC: CCID serial transport. In non-ble builds the console
    // shares this port too (frame parser ignores logs); in ble builds
    // it is CCID-only by construction.
    let usb = UsbSerialDriver::new(
        peripherals.usb_serial,
        peripherals.pins.gpio18,
        peripherals.pins.gpio19,
        &UsbConfig::new().rx_buffer_size(1024).tx_buffer_size(1024),
    )
    .expect("USB-CDC init failed");
    let mut usb = usb;
    // The #91 ring/drain machinery is retired (issue #131): after the
    // pre-claim ceiling above, the app writes only framed CCID to this
    // port. "USB-CDC ready" survives only in `ble` builds (it lands in
    // the BLE ring there; it is filtered to nothing here).
    log::warn!("pn7160-ccid: USB-CDC ready");

    let bus = BusPins {
        i2c0: peripherals.i2c0,
        sda: peripherals.pins.gpio4,
        scl: peripherals.pins.gpio5,
        irq: peripherals.pins.gpio6,
        ven: peripherals.pins.gpio7,
    };

    // ACK-window bring-up (AGENTS.md "PN7160 ACK Window"): the constructor
    // cycles VEN and probes immediately; on ladder failure re-cycle + retry
    // (the chip only ACKs when talked to right after VEN rise).
    let transport =
        EspPn7160Transport::bringup_config_order(pn7160_nci::esp_transport::NUCULA, bus)
            .expect("peripheral init");
    let mut transport = Some(transport);
    let mut driver: Option<Pn7160NfcDriver<EspPn7160Transport>> = None;
    for attempt in 1..=INIT_ATTEMPTS {
        let mut d = Pn7160NfcDriver::new(transport.take().expect("transport"));
        match d.init() {
            Ok(()) => {
                log::warn!("pn7160-ccid: PN7160 initialized (attempt {})", attempt);
                driver = Some(d);
                break;
            }
            Err(e) => {
                log::warn!(
                    "pn7160-ccid: init attempt {} failed ({:?}) — VEN re-cycle + retry",
                    attempt,
                    e
                );
                let mut t = d.into_transport();
                t.ven_cycle();
                let _ = t.probe(); // immediate probe inside the window
                transport = Some(t);
            }
        }
    }
    let mut driver = driver.unwrap_or_else(|| {
        log::warn!(
            "pn7160-ccid: PN7160 unresponsive after {} attempts — card-absent mode",
            INIT_ATTEMPTS
        );
        Pn7160NfcDriver::new(transport.take().expect("transport"))
    });
    // Issue #156 B: interim CCID time-extension frames during a
    // stalling (WTX) exchange — hook + raw writer registration.
    crate::ccid_serial_server::time_extension::register_writer(write_wire_cdc);
    driver.set_stall_hook(Some(crate::ccid_serial_server::time_extension::stall_hook));

    let poll_interval = TickType::new_millis(CARD_POLL_INTERVAL_MS as u64).ticks() as u32;
    let response_windows = ResponseWindows::from_tick_hz(esp_idf_sys::configTICK_RATE_HZ);
    let mut server = CcidSerialServer::new(
        CcidHandler::new(driver),
        poll_interval,
        response_windows,
        unsafe { esp_idf_sys::xTaskGetTickCount() },
    );
    // Issue #119: burst reads. The old 1-byte buffer cost one usb.read
    // call PER BYTE (13 calls for a 13-byte GetSlotStatus frame, ~23 ms
    // each → flat ~300 ms/command). 64 bytes = one full-speed CDC bulk
    // packet; usb.read returns whatever the driver ring holds (up to the
    // buffer length), so a whole frame lands in one call.
    let mut burst_buf = [0u8; 64];
    let timeout_ticks = TickType::new_millis(UART_RX_TIMEOUT_MS as u64).ticks();
    // #153 arrival floor (Codex round 2 on #158): bytes queued in the
    // CDC ring during a blocking handler are returned by the NEXT read
    // — anchor them at the tick of the last dispatch, never at the
    // drain-time read. An empty read proves the ring idle and raises
    // the floor.
    let mut arrival_floor = unsafe { esp_idf_sys::xTaskGetTickCount() };

    // Purge boot-log bytes so CCID starts clean
    esp_idf_hal::delay::FreeRtos::delay_ms(500);
    let mut drain = [0u8; 256];
    loop {
        match usb.read(&mut drain, TickType::new_millis(1).ticks()) {
            Ok(n) if n > 0 => continue,
            _ => break,
        }
    }
    log::warn!("pn7160-ccid: CCID loop starting");

    loop {
        match usb.read(&mut burst_buf, timeout_ticks) {
            Ok(n) if n > 0 => {
                // One read = one burst: every byte in it shares the
                // floor captured before it.
                let read_end = unsafe { esp_idf_sys::xTaskGetTickCount() };
                let floor = arrival_floor;
                for &byte in &burst_buf[..n] {
                    let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
                    if server.feed_byte(byte, now) == ServeAction::Respond {
                        // #153: the frame may have queued behind the
                        // previous handler — anchor it at the burst's
                        // arrival floor before judging expiry, then drop
                        // responses that outlived the host's read window
                        // instead of poisoning the stream.
                        server.refresh_host_deadline(floor);
                        let post = unsafe { esp_idf_sys::xTaskGetTickCount() };
                        if server.response_expired(post) {
                            server.record_late_response();
                            continue;
                        }
                        let write_timeout = TickType::new_millis(100).ticks();
                        // GemPC Twin: echo the received frame, then the response
                        let _ = usb.write(server.echo(), write_timeout);
                        let _ = usb.write(server.response(), write_timeout);
                        #[cfg(feature = "ble")]
                        if let Some(ble) = ble_console.as_ref() {
                            ble.drain();
                        }
                        // Dump-and-retrieve: escape 0xD1 ack went out — panic
                        // now so the panic handler writes the flash coredump.
                        if server.take_snapshot_request() {
                            panic!("escape 0xD1: diagnostic snapshot requested");
                        }
                    }
                }
                // Codex round 3 on #168: the floor may only advance on
                // evidence the queue drained. A SHORT read could not
                // fill the buffer — plausibly drained at read return, so
                // later bytes are fresh. A FULL read may leave older
                // queued bytes behind (retries piling during a handler
                // past one burst): the floor must stay put or their
                // arrivals would be overstated.
                if n < burst_buf.len() {
                    arrival_floor = read_end;
                }
            }
            _ => {
                // Read idle — background card poll; the empty read also
                // proves the ring drained: later bytes are fresh.
                let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
                arrival_floor = now;
                server.poll_if_due(now);
                #[cfg(feature = "ble")]
                if let Some(ble) = ble_console.as_ref() {
                    ble.drain();
                }
            }
        }
    }
}
