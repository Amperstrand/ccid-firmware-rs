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
use crate::ccid_serial_server::{CcidSerialServer, ServeAction};
use crate::nfc::NfcDriver;
use crate::pn7160_driver::Pn7160NfcDriver;
use crate::pn7160_i2c::{BusPins, EspPn7160Transport};

const UART_RX_TIMEOUT_MS: u32 = 100;
const CARD_POLL_INTERVAL_MS: u32 = 500;
const INIT_ATTEMPTS: u32 = 5;

#[link_section = ".rodata"]
static _BUILD_TAG: &[u8] = b"pn7160-ccid-v3";

pub fn run() -> ! {
    link_patches();
    esp_idf_hal::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::set_max_level(log::LevelFilter::Info);
    log::warn!(
        "FWID pn7160-ccid rev={} build={}",
        env!("FW_GIT_REV"),
        env!("FW_BUILD_TS")
    );
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
    let transport = EspPn7160Transport::bringup_config_order(bus).expect("peripheral init");
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
    let driver = driver.unwrap_or_else(|| {
        log::warn!("pn7160-ccid: PN7160 unresponsive after {} attempts — card-absent mode", INIT_ATTEMPTS);
        Pn7160NfcDriver::new(transport.take().expect("transport"))
    });

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
