//! CCID firmware main for the nucula C3: runs the full CCID handler
//! over the USB-CDC serial port. The PN7160 is initialized best-effort —
//! if it NAKs (known hardware issue), the CCID layer still responds to
//! host queries with "card absent", giving a testable CCID endpoint.
//!
//! Feature: `pn7160-ccid` (implies backend-pn7160 + board-nucula).

use esp_idf_hal::delay::TickType;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::usb_serial::{config::Config as UsbConfig, UsbSerialDriver};

use esp_idf_sys::link_patches;

use crate::ccid_handler::CcidHandler;
use crate::ccid_serial_server::{CcidSerialServer, ServeAction};
use crate::nfc::NfcDriver;
use crate::pn7160_driver::Pn7160NfcDriver;
use crate::pn7160_i2c::{BusPins, EspPn7160Transport};

const UART_RX_TIMEOUT_MS: u32 = 100;
const CARD_POLL_INTERVAL_MS: u32 = 500;

#[link_section = ".rodata"]
static _BUILD_TAG: &[u8] = b"pn7160-ccid-v2";

pub fn run() -> ! {
    link_patches();
    esp_idf_hal::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::set_max_level(log::LevelFilter::Info);
    log::warn!("pn7160-ccid: rust main ALIVE");

    let peripherals = Peripherals::take().expect("peripherals already taken");

    // USB-CDC: both console and CCID serial (frame parser ignores logs)
    let usb = UsbSerialDriver::new(
        peripherals.usb_serial,
        peripherals.pins.gpio18,
        peripherals.pins.gpio19,
        &UsbConfig::new().rx_buffer_size(1024).tx_buffer_size(1024),
    )
    .expect("USB-CDC init failed");
    let mut usb = usb;
    log::warn!("pn7160-ccid: USB-CDC ready");

    // PN7160 best-effort: transport creation is peripheral init only
    // (always succeeds); the NCI ladder in init() may fail if the chip
    // NAKs — CCID then responds with card-absent, which is testable.
    let bus = BusPins {
        i2c0: peripherals.i2c0,
        sda: peripherals.pins.gpio4,
        scl: peripherals.pins.gpio5,
        irq: peripherals.pins.gpio6,
        ven: peripherals.pins.gpio7,
    };

    let transport =
        EspPn7160Transport::bringup_config_order(bus).expect("peripheral init (cannot fail)");
    let mut driver = Pn7160NfcDriver::new(transport);
    match driver.init() {
        Ok(()) => log::warn!("pn7160-ccid: PN7160 initialized"),
        Err(e) => {
            log::warn!(
                "pn7160-ccid: PN7160 init failed ({:?}) — card-absent mode",
                e
            )
        }
    }

    let poll_interval = TickType::new_millis(CARD_POLL_INTERVAL_MS as u64).ticks() as u32;
    let mut server = CcidSerialServer::new(CcidHandler::new(driver), poll_interval, unsafe {
        esp_idf_sys::xTaskGetTickCount()
    });
    let mut byte_buf = [0u8; 1];
    let timeout_ticks = TickType::new_millis(UART_RX_TIMEOUT_MS as u64).ticks();

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
        match usb.read(&mut byte_buf, timeout_ticks) {
            Ok(1) => {
                let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
                if server.feed_byte(byte_buf[0], now) == ServeAction::Respond {
                    let write_timeout = TickType::new_millis(100).ticks();
                    // GemPC Twin: echo the received frame, then the response
                    let _ = usb.write(server.echo(), write_timeout);
                    let _ = usb.write(server.response(), write_timeout);
                }
            }
            _ => {
                // Read idle — background card poll
                let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
                server.poll_if_due(now);
            }
        }
    }
}
