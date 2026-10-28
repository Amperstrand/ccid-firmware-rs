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
use crate::ccid_serial_server::{CcidSerialServer, ResponseWindows};
use crate::cdc_burst::{BurstReadError, BurstWire, CdcBurstServe};

const UART_RX_TIMEOUT_MS: u32 = 100;
const CARD_POLL_INTERVAL_MS: u32 = 500;

/// USB-CDC adapter for the host-testable burst serve loop (issue
/// #119): the driver's read returns whatever its ring holds up to
/// `buf.len()` (the CDC-class contract, see `cdc_burst`), so one call
/// delivers a whole burst — the loop logic itself lives in
/// `ccid-service` behind mock-transport unit tests.
struct CdcWire<'a, 'd> {
    usb: &'a mut UsbSerialDriver<'d>,
}

impl BurstWire for CdcWire<'_, '_> {
    fn read_burst(&mut self, buf: &mut [u8], timeout_ticks: u32) -> Result<usize, BurstReadError> {
        self.usb
            .read(buf, timeout_ticks)
            .map_err(|_| BurstReadError)
    }

    fn write_frame(&mut self, bytes: &[u8]) -> bool {
        // 100 ms write timeout at the driver tick rate — the value the
        // retired inline loop used.
        self.usb
            .write(bytes, esp_idf_hal::delay::TickType::new_millis(100).ticks())
            .is_ok()
    }
}

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
    crate::install_platform_clock();

    // #294 WS2: serve-proven OTA validity — arm the gate; the kernel
    // marks this image valid on the first completed CCID exchange.
    crate::ota_validity::arm();

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
    //
    // boards-as-data split (t_a0ebcf54): this main shares the pins
    // bank with the console wire, so NFC BusPins and the GPIO18/19
    // console tokens come out of ONE board_ctor split — the typed
    // gpio4/5/6/7 live only there, pinned to the GENERATED
    // boards::NUCULA row.
    let (nfc_bus, console_d_m, console_d_p) =
        crate::board_ctor::pn7160::nucula_bus_pins_with_console(peripherals.i2c0, peripherals.pins);
    let usb = UsbSerialDriver::new(
        peripherals.usb_serial,
        console_d_m,
        console_d_p,
        &UsbConfig::new().rx_buffer_size(1024).tx_buffer_size(1024),
    )
    .expect("USB-CDC init failed");
    let mut usb = usb;
    // The #91 ring/drain machinery is retired (issue #131): after the
    // pre-claim ceiling above, the app writes only framed CCID to this
    // port. "USB-CDC ready" survives only in `ble` builds (it lands in
    // the BLE ring there; it is filtered to nothing here).
    log::warn!("pn7160-ccid: USB-CDC ready");

    // boards-as-data seam (t_a0ebcf54): the main reduces to board
    // table + constructor + service spawn. The typed gpio4/5/6/7 and
    // the inline ACK-window ladder that used to live here both moved
    // into `board_ctor::pn7160` — the #426 constructor path (transport
    // VEN-cycle + in-window probe, then the init ladder with VEN
    // re-cycle + immediate link_probe between attempts), reading the
    // GENERATED boards::NUCULA row.
    let (mut driver, _pn7160_ok) = crate::board_ctor::pn7160::bring_up(nfc_bus);
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

    // Issue #119: burst reads (66109dd) — the old 1-byte buffer cost
    // one usb.read call PER BYTE (13 calls for a 13-byte GetSlotStatus
    // frame → the flat ~300 ms/command the battery measured). 64 bytes
    // = one full-speed CDC bulk packet; usb.read returns whatever the
    // driver ring holds, so a whole frame lands in one call. The loop
    // (burst feed, #153/#168 arrival-floor discipline, inline response
    // flush, snapshot stop) lives in ccid-service (`cdc_burst`),
    // host-tested with mock transports — this main only supplies the
    // wire adapter, the tick source, and the wire-quiet hook (BLE
    // drain).
    let mut serve = CdcBurstServe::new(
        server,
        unsafe { esp_idf_sys::xTaskGetTickCount() },
        "gempc-cdc",
    );
    let mut burst_buf = [0u8; 64];
    let read_timeout_ticks = TickType::new_millis(UART_RX_TIMEOUT_MS as u64).ticks();
    // BLE drain runs at the wire-quiet points (post-response, idle);
    // no-op in non-ble builds.
    #[cfg(feature = "ble")]
    let mut on_quiet = || {
        if let Some(ble) = ble_console.as_ref() {
            ble.drain();
        }
    };
    #[cfg(not(feature = "ble"))]
    let mut on_quiet = || {};

    loop {
        let outcome = serve.serve_once(
            &mut CdcWire { usb: &mut usb },
            &mut burst_buf,
            read_timeout_ticks,
            || unsafe { esp_idf_sys::xTaskGetTickCount() },
            &mut on_quiet,
        );
        // Dump-and-retrieve: the 0xD1 ack is on the wire — panic now
        // so the panic handler writes the flash coredump.
        if outcome.snapshot_requested {
            panic!("escape 0xD1: diagnostic snapshot requested");
        }
    }
}
