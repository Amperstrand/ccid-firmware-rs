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
                log::warn!(
                    "vpcd: disconnected — reconnecting in {}ms",
                    RECONNECT_DELAY_MS
                );
            }
            Some(Err(e)) => {
                log::warn!(
                    "vpcd: connect failed ({}) — retrying in {}ms",
                    e,
                    RECONNECT_DELAY_MS
                );
            }
            None => {
                log::warn!(
                    "vpcd: host resolve failed — retrying in {}ms",
                    RECONNECT_DELAY_MS
                );
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
    // #328/#332 (Codex P1): the idle wake doubles as the maintenance
    // tick — a 5 s read timeout keeps the wake cheap while giving the
    // parked re-arm a time base that does NOT depend on host polls
    // (pcscd stops probing entirely once a reader is marked dead).
    // #349 (Codex P1, 00:14): the clock origin is PROCESS-WIDE — the
    // handler and its driver SURVIVE reconnects in client_loop, so a
    // connection-local Instant would reset now_ms to ~0 against a
    // last_rearm_ms from the previous connection's domain (saturating
    // subtraction pinned at zero → re-arms suppressed for the new
    // connection's whole lifetime).
    static CLOCK_ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let started = *CLOCK_ORIGIN.get_or_init(std::time::Instant::now);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
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
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                // Idle wake: advance the driver's parked re-arm by TIME
                // (one bounded step; at most one write per 12.8 s
                // window — the slow cadence keeps the rapid-cycle mute
                // class out of reach).
                handler
                    .driver_mut()
                    .poll_maintenance(started.elapsed().as_millis() as u64);
                continue;
            }
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
                let present = d.is_card_present();
                if present {
                    let mut atr = [0u8; 64];
                    match d.power_on(&mut atr) {
                        Ok(len) => {
                            // #326 observability: the ATR exchange is the
                            // first thing that dies on this path — every
                            // request and its verdict goes to the console.
                            log::warn!("vpcd: ATR req → present, power_on ok ({} B)", len);
                            Some(atr[..len].to_vec())
                        }
                        Err(_) => {
                            log::warn!("vpcd: ATR req → present, power_on FAILED → empty");
                            Some(Vec::new()) // power failed → "no card"
                        }
                    }
                } else {
                    log::warn!("vpcd: ATR req → NOT present → empty (driver re-polls)");
                    Some(Vec::new()) // no card → empty ATR (driver ejects + re-polls)
                }
            }
            VPCD_CTRL_ON => {
                let mut atr = [0u8; 64];
                let _ = handler.driver_mut().power_on(&mut atr);
                log::warn!("vpcd: ON ctrl");
                None // fire-and-forget
            }
            VPCD_CTRL_OFF => {
                handler.driver_mut().power_off();
                log::warn!("vpcd: OFF ctrl");
                None // fire-and-forget
            }
            VPCD_CTRL_RESET => {
                handler.driver_mut().power_off();
                let mut atr = [0u8; 64];
                match handler.driver_mut().power_on(&mut atr) {
                    Ok(len) => {
                        log::warn!("vpcd: RESET ctrl → ATR ({} B)", len);
                        Some(atr[..len].to_vec())
                    }
                    Err(_) => {
                        log::warn!("vpcd: RESET ctrl → power_on FAILED → empty");
                        Some(Vec::new()) // reset failed → "no card"
                    }
                }
            }
            other => {
                log::warn!("vpcd: unknown ctrl 0x{:02x}", other);
                None
            }
        }
    } else if !payload.is_empty() {
        match handler.driver_mut().transmit_apdu(payload, rx_buf) {
            Ok(len) => Some(rx_buf[..len].to_vec()),
            Err(_) => {
                // #170: live session + Err = transient RF fade → 64 00
                // (pcscd retries in place). Dead session (the #156
                // resync teardown already ran) → 0-length = vpcd.h
                // "ICC absent": the driver ejects + re-polls and the
                // next ATR rebuilds from a fresh activation. 64 00 on
                // a dead session wedges the reader until board reboot.
                if handler.driver_mut().session_active() {
                    log::warn!("vpcd: APDU err on LIVE session → 6400");
                    Some(crate::ccid_handler::CARD_TIMEOUT_SW.to_vec())
                } else {
                    log::warn!("vpcd: APDU err on DEAD session → ICC absent");
                    Some(Vec::new())
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nfc::NfcDriver;

    #[derive(Debug)]
    enum MockError {
        Failed,
    }

    /// Scripted NfcDriver for #170: records the exact APDU bytes the
    /// relay forwards and replays a scripted response, failure, and
    /// session-liveness state.
    struct ScriptedDriver {
        response: Vec<u8>,
        fail: bool,
        session: bool,
        last_apdu: Vec<u8>,
    }

    impl NfcDriver for ScriptedDriver {
        type Error = MockError;

        fn init(&mut self) -> Result<(), MockError> {
            Ok(())
        }
        fn is_card_present(&mut self) -> bool {
            true
        }
        fn power_on(&mut self, _atr_buf: &mut [u8]) -> Result<usize, MockError> {
            Ok(0)
        }
        fn power_off(&mut self) {}
        fn transmit_apdu(
            &mut self,
            command: &[u8],
            response: &mut [u8],
        ) -> Result<usize, MockError> {
            self.last_apdu = command.to_vec();
            if self.fail {
                return Err(MockError::Failed);
            }
            response[..self.response.len()].copy_from_slice(&self.response);
            Ok(self.response.len())
        }
        fn session_active(&self) -> bool {
            self.session
        }
    }

    fn harness(
        driver: ScriptedDriver,
    ) -> (crate::ccid_handler::CcidHandler<ScriptedDriver>, [u8; 1024]) {
        (crate::ccid_handler::CcidHandler::new(driver), [0u8; 1024])
    }

    /// #170 bench sequence, relay side: the bare 6-byte zero-data APDU
    /// (`B0 B0 00 00 00 00`, Lc=0 before Le) must be forwarded verbatim
    /// — the relay never length-parses the APDU — and a clean card
    /// answer (SW 6700 from the card's own length check) passes back
    /// untouched with the session intact.
    #[test]
    fn zero_data_apdu_relays_verbatim_and_keeps_session() {
        let driver = ScriptedDriver {
            response: vec![0x67, 0x00],
            fail: false,
            session: true,
            last_apdu: Vec::new(),
        };
        let (mut handler, mut rx_buf) = harness(driver);
        let resp = handle_message(
            &mut handler,
            &[0xB0, 0xB0, 0x00, 0x00, 0x00, 0x00],
            &mut rx_buf,
        )
        .expect("response expected");
        assert_eq!(resp, vec![0x67, 0x00]);
        assert_eq!(
            handler.driver_mut().last_apdu,
            vec![0xB0, 0xB0, 0x00, 0x00, 0x00, 0x00],
            "the relay must forward the 6-byte form untouched"
        );
    }

    /// Transient failure on a live session keeps the historic contract:
    /// SW 6400 so pcscd retries in place.
    #[test]
    fn transient_failure_on_live_session_keeps_6400() {
        let driver = ScriptedDriver {
            response: Vec::new(),
            fail: true,
            session: true,
            last_apdu: Vec::new(),
        };
        let (mut handler, mut rx_buf) = harness(driver);
        let resp = handle_message(&mut handler, &[0xB0, 0xB0, 0x00, 0x00, 0x00], &mut rx_buf)
            .expect("response expected");
        assert_eq!(resp, vec![0x64, 0x00]);
    }

    /// #170 wedge heal: a session-fatal failure (the #156 resync
    /// teardown already ran — `session_active() == false`) must answer
    /// 0-length, the vpcd.h "ICC absent" signal: the driver ejects and
    /// re-polls, the next ATR request rebuilds from a fresh activation.
    /// 6400 here is the observed reader-wedge (dead session served as
    /// retryable until board reboot).
    #[test]
    fn session_fatal_failure_answers_icc_absent() {
        let driver = ScriptedDriver {
            response: Vec::new(),
            fail: true,
            session: false,
            last_apdu: Vec::new(),
        };
        let (mut handler, mut rx_buf) = harness(driver);
        let resp = handle_message(
            &mut handler,
            &[0xB0, 0xB0, 0x00, 0x00, 0x00, 0x00],
            &mut rx_buf,
        )
        .expect("response expected");
        assert!(
            resp.is_empty(),
            "session-fatal failure must answer ICC-absent, got {resp:?}"
        );
    }
}
