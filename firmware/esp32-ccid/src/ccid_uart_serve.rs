//! Shared UART transport loop for the GemPC Twin serial CCID service.
//!
//! One iteration = one byte read (or read timeout) + all resulting wire
//! writes. The protocol state machine lives in
//! [`crate::ccid_serial_server::CcidSerialServer`]; this module owns only
//! the UART hardware concerns (byte reads with timeout, write-all with
//! logging, boot-drain). Both UART mains (MFRC522 boards, PN532 devkits)
//! drive their serving loop through [`serve_once`] so the wire behavior
//! cannot drift between backends. Hardware reactions that differ per main
//! (LEDs, BLE log drain) stay in the mains, keyed off the returned event.

use crate::ccid_serial_server::{CcidSerialServer, MalformedFramePolicy, PollOutcome, ServeAction};
use crate::nfc::NfcDriver;
use ccid_transport_serial::{build_nak_frame, build_slot_change_notification};
use esp_idf_hal::uart::UartDriver;
use esp_idf_sys::EspError;

/// GemPC Twin read timeout: matches libccidtwin expectations for a
/// response after the echo bytes.
pub const UART_RX_TIMEOUT_MS: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeEvent {
    /// Nothing happened (partial frame byte consumed, or parse error
    /// under the drop policy).
    None,
    /// A complete command was answered (echo [+ in-band notification] +
    /// response written to the wire).
    Responded,
    /// A malformed frame was NAKed (Nak policy only).
    Nak,
    /// The read timed out and the idle poll ran; `card_change` is
    /// `Some(present)` when presence flipped. Mains that suppress
    /// unsolicited notifications ignore the value (state was still
    /// refreshed inside the server).
    IdlePolled { card_change: Option<bool> },
    /// The read timed out; poll interval not yet elapsed. Partial-frame
    /// parser state is discarded here (parity with the historical
    /// mains, which reset their frame buffer on every read timeout).
    Idle,
}

fn write_all(uart: &UartDriver, mut bytes: &[u8]) -> Result<(), EspError> {
    while !bytes.is_empty() {
        let written = uart.write(bytes)?;
        if written == 0 {
            continue;
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

fn write_all_logged(uart: &UartDriver, bytes: &[u8]) {
    if let Err(e) = write_all(uart, bytes) {
        log::error!("UART write failed: {:?}", e);
    }
}

/// Serve one interaction on `uart`. Blocks for at most one read timeout.
pub fn serve_once<D: NfcDriver>(
    uart: &UartDriver,
    server: &mut CcidSerialServer<D>,
    timeout_ticks: esp_idf_hal::delay::TickType::ticks(),
) -> ServeEvent {
    let mut byte_buf = [0u8; 1];
    match uart.read(&mut byte_buf, timeout_ticks) {
        Ok(1) => {
            let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
            match server.feed_byte(byte_buf[0], now) {
                ServeAction::Respond => {
                    write_all_logged(uart, server.echo());
                    if let Some(present) = server.take_inband_notification() {
                        let mut notif = [0u8; 2];
                        let len = build_slot_change_notification(present, &mut notif);
                        write_all_logged(uart, &notif[..len]);
                    }
                    write_all_logged(uart, server.response());
                    ServeEvent::Responded
                }
                ServeAction::Nak => {
                    let mut nak = [0u8; 3];
                    let len = build_nak_frame(&mut nak);
                    write_all_logged(uart, &nak[..len]);
                    ServeEvent::Nak
                }
                ServeAction::None => ServeEvent::None,
            }
        }
        _ => {
            server.abort_partial_frame();
            let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
            match server.poll_if_due(now) {
                PollOutcome::Polled { card_change } => ServeEvent::IdlePolled { card_change },
                PollOutcome::NotPolled => ServeEvent::Idle,
            }
        }
    }
}

/// Purge stale UART bytes (boot log remnants) so the protocol starts
/// clean — pcscd expects the first SYNC from the host.
pub fn drain_uart(uart: &UartDriver) {
    esp_idf_hal::delay::FreeRtos::delay_ms(500);
    let _ = uart.wait_tx_done(esp_idf_hal::delay::TickType::new_millis(100).into());
    let mut drain = [0u8; 256];
    loop {
        match uart.read(&mut drain, 1) {
            Ok(n) if n > 0 => continue,
            _ => break,
        }
    }
}

/// Convenience: the UART mains' NAK policy (libccidtwin-verified).
pub const UART_POLICY: MalformedFramePolicy = MalformedFramePolicy::Nak;
