//! VPCD-protocol client for the nucula — REVERSED connection mode.
//! Connects OUT to the host's vpcd driver listener (:35963) instead of
//! listening. The stock `libifdvpcd.so` stays in server mode (the only
//! mode that doesn't crash pcscd on this system — client mode triggers
//! a glibc FD_SETSIZE abort). Same pattern as vsmartcard's remote-reader
//! Android app. Reconnects automatically when the host restarts pcscd.
//!
//! VPCD_PROTOCOL (from vsmartcard/src/vpcd/vpcd.c):
//!   All messages: 2-byte big-endian length prefix + payload.
//!   Control messages (payload = exactly 1 byte, vpcd.h):
//!     0x00 = OFF, 0x01 = ON, 0x02 = RESET, 0x04 = ATR
//!   ATR and RESET expect responses; ON/OFF are fire-and-forget.
//!   APDU exchange: payload = full C-APDU → response = full R-APDU (incl SW).
//!   0-length response = "ICC absent": the driver ejects + re-polls. Used
//!   deliberately for no-card / failed power; never for APDU errors (those
//!   answer SW 6400 so the session survives transient RF drops).
//!
//! Feature: `wifi-vpcd` (the primary wifi path).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::nfc::NfcDriver;

/// VPCDPORT from vsmartcard/src/vpcd/vpcd.h — the host-side driver listens here
pub const HOST_VPCD_PORT: u16 = 35963;
const RECONNECT_DELAY_MS: u64 = 3000;

#[link_section = ".rodata"]
static _VPCD_BUILD_TAG: &[u8] = b"vpcd-rev-v3";

// VPCD_CTRL: control byte values from vsmartcard/src/vpcd/vpcd.h
const VPCD_CTRL_OFF: u8 = 0x00;
const VPCD_CTRL_ON: u8 = 0x01;
const VPCD_CTRL_RESET: u8 = 0x02;
const VPCD_CTRL_ATR: u8 = 0x04;

/// Spawn the vpcd client thread. Takes the NFC driver exclusively.
/// `port` selects the driver slot: the stock libifdvpcd listens on
/// :35963 (slot 00 00) and :35964 (slot 00 01) — give each board its
/// own port so a multi-board bench maps boards to readers
/// deterministically instead of racing one listener.
pub fn spawn<D: NfcDriver + Send + 'static>(driver: D, host_ip: &str, port: u16) {
    let host = host_ip.to_string();
    let _ = std::thread::Builder::new()
        .stack_size(16 * 1024)
        .name("vpcd".into())
        .spawn(move || client_loop(driver, &host, port));
}

fn client_loop<D: NfcDriver + Send + 'static>(driver: D, host: &str, port: u16) {
    let mut handler = crate::ccid_handler::CcidHandler::new(driver);
    let mut rx_buf = [0u8; 1024];

    loop {
        log::warn!("vpcd: connecting to {}:{}", host, port);
        // connect_timeout, not connect: lwIP's connect() has no built-in
        // timeout — a SYN blackholed by a host firewall (ufw DROP) blocks
        // this thread indefinitely instead of reaching the retry loop.
        // 5s bounds one attempt.
        let attempt = (host, port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut i| i.next())
            .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(5)));
        match attempt {
            Some(Ok(mut stream)) => {
                let _ = stream.set_nodelay(true);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
                log::warn!("vpcd: connected to host {}", host);
                serve_connection(&mut stream, &mut handler, &mut rx_buf);
                log::warn!("vpcd: disconnected — reconnecting in {}ms", RECONNECT_DELAY_MS);
            }
            Some(Err(e)) => {
                log::warn!("vpcd: connect failed ({}) — retrying in {}ms", e, RECONNECT_DELAY_MS);
            }
            None => {
                log::warn!("vpcd: host resolve failed — retrying in {}ms", RECONNECT_DELAY_MS);
            }
        }
        std::thread::sleep(Duration::from_millis(RECONNECT_DELAY_MS));
    }
}

fn serve_connection<D: NfcDriver>(
    stream: &mut TcpStream,
    handler: &mut crate::ccid_handler::CcidHandler<D>,
    rx_buf: &mut [u8],
) {
    loop {
        let payload = match read_message(stream) {
            Ok(Some(p)) => p,
            Ok(None) => return, // host closed the stream (EOF)
            // Read timeout while idle: vpcd is pure request/response — a
            // silent host (no active PC/SC clients) is normal, not a dead
            // link. Disconnecting here churned the TCP connection every
            // 30s, re-rolling which driver slot wins accept() (two
            // listeners, :35963/:35964) and wedging CLOSE-WAIT sockets
            // on the host. Field-observed as the slot-flapping cause.
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => continue,
            Err(_) => return, // hard I/O error — reconnect
        };
        let response = handle_message(handler, &payload, rx_buf);
        if let Some(resp) = response {
            if send_message(stream, &resp).is_err() {
                return;
            }
        }
    }
}

fn handle_message<D: NfcDriver>(
    handler: &mut crate::ccid_handler::CcidHandler<D>,
    payload: &[u8],
    rx_buf: &mut [u8],
) -> Option<Vec<u8>> {
    if payload.len() == 1 {
        match payload[0] {
            VPCD_CTRL_ATR => {
                let d = handler.driver_mut();
                if d.is_card_present() {
                    let mut atr = [0u8; 64];
                    match d.power_on(&mut atr) {
                        Ok(len) => Some(atr[..len].to_vec()),
                        Err(_) => Some(Vec::new()), // power failed → "no card"
                    }
                } else {
                    Some(Vec::new()) // no card → empty ATR (driver ejects + re-polls)
                }
            }
            VPCD_CTRL_ON => {
                let mut atr = [0u8; 64];
                let _ = handler.driver_mut().power_on(&mut atr);
                None // fire-and-forget
            }
            VPCD_CTRL_OFF => {
                handler.driver_mut().power_off();
                None // fire-and-forget
            }
            VPCD_CTRL_RESET => {
                handler.driver_mut().power_off();
                let mut atr = [0u8; 64];
                match handler.driver_mut().power_on(&mut atr) {
                    Ok(len) => Some(atr[..len].to_vec()),
                    Err(_) => Some(Vec::new()), // reset failed → "no card"
                }
            }
            _ => None,
        }
    } else if !payload.is_empty() {
        match handler.driver_mut().transmit_apdu(payload, rx_buf) {
            Ok(len) => Some(rx_buf[..len].to_vec()),
            Err(_) => Some(vec![0x64, 0x00]),
        }
    } else {
        None
    }
}

fn read_message(stream: &mut impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 2];
    match stream.read_exact(&mut len_buf) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u16::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(Some(Vec::new()));
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    Ok(Some(payload))
}

fn send_message(stream: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len() as u16;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}
