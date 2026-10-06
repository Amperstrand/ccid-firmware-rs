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
use crate::nfc::NfcDriver;
use crate::pn7160_driver::Pn7160NfcDriver;
use crate::pn7160_i2c::{BusPins, EspPn7160Transport};

use ccid_protocol::types::PC_TO_RDR_GET_SLOT_STATUS;
use ccid_transport_serial::{FrameEvent, FrameParser};

const MAX_FRAME_SIZE: usize = 512;
const MAX_CCID_RESPONSE_SIZE: usize = 271;
const UART_RX_TIMEOUT_MS: u32 = 100;
const CARD_POLL_INTERVAL_MS: u32 = 500;

#[link_section = ".rodata"]
static _BUILD_TAG: &[u8] = b"pn7160-ccid-v1";

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

    let mut ccid_handler = CcidHandler::new(driver);
    let mut frame_parser = FrameParser::new();
    let mut frame_buf = [0u8; MAX_FRAME_SIZE];
    let mut frame_len = 0usize;
    let mut byte_buf = [0u8; 1];
    let timeout_ticks = TickType::new_millis(UART_RX_TIMEOUT_MS as u64).ticks();
    let poll_interval = TickType::new_millis(CARD_POLL_INTERVAL_MS as u64).ticks() as u32;
    let mut last_poll_tick: u32 = unsafe { esp_idf_sys::xTaskGetTickCount() };

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
                let byte = byte_buf[0];

                if frame_len < frame_buf.len() {
                    frame_buf[frame_len] = byte;
                    frame_len += 1;
                } else {
                    let mut nak = [0u8; 4];
                    nak[0] = ccid_transport_serial::SYNC;
                    nak[1] = ccid_transport_serial::CTRL_NAK;
                    let _ = usb.write(&nak, TickType::new_millis(100).ticks());
                    ccid_handler.record_nak();
                    frame_len = 0;
                    frame_parser.reset();
                    continue;
                }

                match frame_parser.feed(byte) {
                    Some(FrameEvent::Command { ccid_bytes }) => {
                        // Echo (GemPC Twin protocol)
                        let _ =
                            usb.write(&frame_buf[..frame_len], TickType::new_millis(100).ticks());

                        // Time-gated card poll on GetSlotStatus
                        let is_slot_status = ccid_bytes.first() == Some(&PC_TO_RDR_GET_SLOT_STATUS);
                        if is_slot_status {
                            let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
                            if now.wrapping_sub(last_poll_tick) >= poll_interval {
                                last_poll_tick = now;
                                ccid_handler.check_card_change();
                            }
                        }

                        let mut resp_buf = [0u8; MAX_CCID_RESPONSE_SIZE];
                        let resp_len = ccid_handler.process_command(&ccid_bytes, &mut resp_buf);

                        // Frame the response: SYNC + ACK + data + LRC
                        let mut frame_out = [0u8; MAX_FRAME_SIZE];
                        frame_out[0] = ccid_transport_serial::SYNC;
                        frame_out[1] = ccid_transport_serial::CTRL_ACK;
                        frame_out[2..2 + resp_len].copy_from_slice(&resp_buf[..resp_len]);
                        let mut lrc = 0u8;
                        for &b in &frame_out[..2 + resp_len] {
                            lrc ^= b;
                        }
                        frame_out[2 + resp_len] = lrc;
                        let out_len = 2 + resp_len + 1;
                        let _ = usb.write(&frame_out[..out_len], TickType::new_millis(100).ticks());

                        frame_len = 0;
                        frame_parser.reset();
                    }
                    _ => {}
                }
            }
            _ => {
                // UART idle — background card poll
                let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
                if now.wrapping_sub(last_poll_tick) >= poll_interval {
                    last_poll_tick = now;
                    ccid_handler.check_card_change();
                }
            }
        }
    }
}
