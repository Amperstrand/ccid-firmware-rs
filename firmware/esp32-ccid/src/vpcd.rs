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
    // #326 (core-dump 2026-10-21): 16 KiB is NOT enough for the APDU
    // exchange path — the first transmit_apdu drove this thread to
    // 16384/8 bytes used/free and the next interrupt landed on the
    // smashed stack (panic in interrupt context, task 'pthread').
    // The serial image runs the identical exchange on a task with
    // ~31 KiB free, which is why only wifi-vpcd crashed. 32 KiB = 2x
    // the observed high-water mark.
    let _ = std::thread::Builder::new()
        .stack_size(32 * 1024)
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
    // #326: last successful ATR, cached so the 1.2 s presence probes
    // can answer WITHOUT re-running power_on (which re-activates and
    // thereby DESELECTS the card — the churn that made the first APDU
    // after connect take 1.6 s of T=1 resync and blow every host-side
    // response-pairing race).
    let mut cached_atr: Vec<u8> = Vec::new();
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
        // #326 discriminator: log EVERY request kind and answer with a
        // millisecond stamp so console and tcpdump correlate 1:1.
        let t0 = std::time::Instant::now();
        // #360 review (P1): the payload hex dump is GONE — a VERIFY
        // (00 20 00 81 Lc…) puts PIN bytes inside the old first-8
        // slice, and bench-net broadcasts WARN records over UDP.
        let was_apdu = payload.len() != 1;
        log::warn!("vpcd req: {}", request_log_line(&payload));
        let response = handle_message(handler, &payload, rx_buf, &mut cached_atr);
        if let Some(resp) = response {
            log::warn!(
                "vpcd ans (+{}ms): {}",
                t0.elapsed().as_millis(),
                response_log_line(&resp, was_apdu)
            );
            if send_message(stream, &resp).is_err() {
                return;
            }
        }
    }
}

/// #360 review (P1): request log line. NEVER include APDU payload
/// bytes — VERIFY (`00 20 00 81 Lc …`) carries PIN digits in the first
/// eight, CHANGE REFERENCE DATA likewise, and bench-net broadcasts
/// WARN records over UDP to the bench subnet. Direction + length only;
/// control bytes are protocol state, not secrets, and stay logged.
fn request_log_line(payload: &[u8]) -> String {
    if payload.len() == 1 {
        format!("ctrl 0x{:02x}", payload[0])
    } else {
        format!("APDU {}B [redacted]", payload.len())
    }
}

/// #360 review (P1): response log line. Response DATA stays out for
/// the same reason (GET RESPONSE / READ BINARY carry card content);
/// the SW pair of an R-APDU is protocol status, not card data, so it
/// is safe. Control responses (ATR probes, RESET) log length only.
fn response_log_line(resp: &[u8], was_apdu: bool) -> String {
    if was_apdu && resp.len() >= 2 {
        let n = resp.len();
        format!("{}B SW {:02x}{:02x}", n, resp[n - 2], resp[n - 1])
    } else {
        format!("{}B", resp.len())
    }
}

fn handle_message<D: NfcDriver>(
    handler: &mut crate::ccid_handler::CcidHandler<D>,
    payload: &[u8],
    rx_buf: &mut [u8],
    cached_atr: &mut Vec<u8>,
) -> Option<Vec<u8>> {
    if payload.len() == 1 {
        match payload[0] {
            VPCD_CTRL_ATR => {
                // #326: with a LIVE session answer from the cache —
                // power_on here would re-activate (and deselect) the
                // card on every 1.2 s presence probe.
                // #360 review (P1): poll presence BEFORE serving the
                // cache. On the PN7160 path a card removed mid-session
                // keeps session_active() true until is_card_present()
                // drains the pending RF_DEACTIVATE_NTF; skipping the
                // poll let the cache answer forever, so pcscd could
                // not observe the removal until it sent an APDU. The
                // check is non-blocking and does NOT reactivate.
                let present = handler.driver_mut().is_card_present();
                if present && handler.driver_mut().session_active() && !cached_atr.is_empty() {
                    return Some(cached_atr.clone());
                }
                let d = handler.driver_mut();
                if present {
                    let mut atr = [0u8; 64];
                    match d.power_on(&mut atr) {
                        Ok(len) => {
                            *cached_atr = atr[..len].to_vec();
                            log::warn!("vpcd: ATR req → present, power_on ok ({} B)", len);
                            Some(atr[..len].to_vec())
                        }
                        Err(_) => {
                            log::warn!("vpcd: ATR req → present, power_on FAILED → empty");
                            Some(Vec::new()) // power failed → "no card"
                        }
                    }
                } else {
                    // card gone (or never inserted): the cache no longer
                    // describes what is in the field — drop it so a
                    // later probe re-runs a fresh activation.
                    *cached_atr = Vec::new();
                    log::warn!("vpcd: ATR req → NOT present → empty (driver re-polls)");
                    Some(Vec::new()) // no card → empty ATR (driver ejects + re-polls)
                }
            }
            VPCD_CTRL_ON => {
                // #360 review (P2): refresh the cache from the
                // activation — ON after a card swap must not leave
                // card A's ATR cached for card B's session.
                let mut atr = [0u8; 64];
                match handler.driver_mut().power_on(&mut atr) {
                    Ok(len) => *cached_atr = atr[..len].to_vec(),
                    Err(_) => cached_atr.clear(),
                }
                log::warn!("vpcd: ON ctrl");
                None // fire-and-forget
            }
            VPCD_CTRL_OFF => {
                handler.driver_mut().power_off();
                // #360 review (P2): OFF must invalidate — the card can
                // be swapped out before the next ON.
                cached_atr.clear();
                log::warn!("vpcd: OFF ctrl");
                None // fire-and-forget
            }
            VPCD_CTRL_RESET => {
                handler.driver_mut().power_off();
                let mut atr = [0u8; 64];
                match handler.driver_mut().power_on(&mut atr) {
                    Ok(len) => {
                        *cached_atr = atr[..len].to_vec();
                        log::warn!("vpcd: RESET ctrl → ATR ({} B)", len);
                        Some(atr[..len].to_vec())
                    }
                    Err(_) => {
                        // #360 review (P2): failed re-activation
                        // invalidates — never serve the pre-RESET ATR.
                        cached_atr.clear();
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
    /// #360 review: `atrs` scripts successive power_on activations
    /// (each call pops the front; an empty list fails power_on — the
    /// "activation failure" case), `present` scripts presence, and the
    /// counters prove which driver entry points handle_message used.
    struct ScriptedDriver {
        response: Vec<u8>,
        fail: bool,
        session: bool,
        last_apdu: Vec<u8>,
        present: bool,
        atrs: Vec<Vec<u8>>,
        presence_calls: usize,
        power_on_calls: usize,
    }

    impl NfcDriver for ScriptedDriver {
        type Error = MockError;

        fn init(&mut self) -> Result<(), MockError> {
            Ok(())
        }
        fn is_card_present(&mut self) -> bool {
            self.presence_calls += 1;
            self.present
        }
        fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, MockError> {
            self.power_on_calls += 1;
            let Some(atr) = self.atrs.first().cloned() else {
                return Err(MockError::Failed);
            };
            self.atrs.remove(0);
            atr_buf[..atr.len()].copy_from_slice(&atr);
            Ok(atr.len())
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
            present: true,
            atrs: Vec::new(),
            presence_calls: 0,
            power_on_calls: 0,
        };
        let (mut handler, mut rx_buf) = harness(driver);
        let mut cached_atr = Vec::new();
        let resp = handle_message(
            &mut handler,
            &[0xB0, 0xB0, 0x00, 0x00, 0x00, 0x00],
            &mut rx_buf,
            &mut cached_atr,
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
            present: true,
            atrs: Vec::new(),
            presence_calls: 0,
            power_on_calls: 0,
        };
        let (mut handler, mut rx_buf) = harness(driver);
        let mut cached_atr = Vec::new();
        let resp = handle_message(
            &mut handler,
            &[0xB0, 0xB0, 0x00, 0x00, 0x00],
            &mut rx_buf,
            &mut cached_atr,
        )
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
            present: true,
            atrs: Vec::new(),
            presence_calls: 0,
            power_on_calls: 0,
        };
        let (mut handler, mut rx_buf) = harness(driver);
        let mut cached_atr = Vec::new();
        let resp = handle_message(
            &mut handler,
            &[0xB0, 0xB0, 0x00, 0x00, 0x00, 0x00],
            &mut rx_buf,
            &mut cached_atr,
        )
        .expect("response expected");
        assert!(
            resp.is_empty(),
            "session-fatal failure must answer ICC-absent, got {resp:?}"
        );
    }

    /// #360 review (P1) "poll for link loss before serving the cached
    /// ATR": a live session + populated cache must still consult
    /// presence — on the PN7160 path that check drains the pending
    /// RF_DEACTIVATE_NTF after mid-session card removal. Without it the
    /// cache answers forever and pcscd cannot see the removal. The poll
    /// must also NOT reactivate (power_on) — that deselect-churn is what
    /// the cache exists to avoid.
    #[test]
    fn atr_fast_path_polls_presence_and_never_reactivates() {
        let driver = ScriptedDriver {
            response: Vec::new(),
            fail: false,
            session: true,
            last_apdu: Vec::new(),
            present: true,
            atrs: Vec::new(),
            presence_calls: 0,
            power_on_calls: 0,
        };
        let (mut handler, mut rx_buf) = harness(driver);
        let cached = vec![0x3b, 0x00]; // card A's ATR, session live
        let mut cached_atr = cached.clone();

        // card still present → cached ATR served, no activation
        let resp = handle_message(&mut handler, &[VPCD_CTRL_ATR], &mut rx_buf, &mut cached_atr)
            .expect("ATR probe must answer");
        assert_eq!(resp, cached);
        assert!(
            handler.driver_mut().power_on_calls == 0,
            "the presence poll must not reactivate the card"
        );
        assert!(
            handler.driver_mut().presence_calls >= 1,
            "the ATR fast path must consult presence"
        );

        // card removed mid-session (session_active() still true until the
        // presence check drains the deactivate notification) → the probe
        // must answer ICC-absent, not the stale cache
        handler.driver_mut().present = false;
        let resp = handle_message(&mut handler, &[VPCD_CTRL_ATR], &mut rx_buf, &mut cached_atr)
            .expect("ATR probe must answer");
        assert!(
            resp.is_empty(),
            "card loss must surface as ICC-absent on the very next probe, got {resp:?}"
        );
        assert!(cached_atr.is_empty(), "absence must drop the stale cache");
        assert!(
            handler.driver_mut().power_on_calls == 0,
            "absence handling must not attempt an activation"
        );
    }

    /// #360 review (P1) "redact APDU data from request logs": the old
    /// line printed the first 8 payload bytes — for VERIFY
    /// (`00 20 00 81 Lc …`) that is CLA INS P1 P2 Lc + the first three
    /// PIN digits — and bench-net broadcasts WARN records over UDP.
    /// The request log must carry direction + length only.
    #[test]
    fn apdu_request_log_redacts_payload_bytes() {
        // VERIFY, PIN "12345678" — the exact command the review flagged
        let verify = [
            0x00, 0x20, 0x00, 0x81, 0x08, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38,
        ];
        let line = request_log_line(&verify);
        assert_eq!(line, "APDU 13B [redacted]");
        for b in verify {
            assert!(
                !line.contains(&format!("{b:02x}")),
                "payload byte {b:02x} leaked into the request log: {line}"
            );
        }
        // control bytes are protocol state, not credentials — unchanged
        assert_eq!(request_log_line(&[VPCD_CTRL_ATR]), "ctrl 0x04");
        assert_eq!(request_log_line(&[VPCD_CTRL_ON]), "ctrl 0x01");
    }

    /// #360 review (P1), response side: R-APDU DATA (GET RESPONSE, READ
    /// BINARY, …) must stay out of the log; the SW pair is protocol
    /// status and may appear. Control responses (ATR probes) log length
    /// only.
    #[test]
    fn apdu_response_log_shows_sw_only() {
        let rapdu = [0x61, 0x02, 0x31, 0x32, 0x33, 0x34, 0x90, 0x00];
        let line = response_log_line(&rapdu, true);
        assert_eq!(line, "8B SW 9000");
        assert!(
            !line.contains("31323334") && !line.contains("6102"),
            "response data leaked into the log: {line}"
        );
        assert_eq!(response_log_line(&[0x64, 0x00], true), "2B SW 6400");
        assert_eq!(response_log_line(&[], true), "0B"); // ICC absent
        assert_eq!(response_log_line(&[0x3b, 0x00], false), "2B"); // ATR: length only
    }

    /// #360 review (P2) "refresh the cached ATR after lifecycle
    /// controls": ON card A → OFF → ON card B must make the next ATR
    /// probe serve card B's ATR. Before the fix, ON/OFF left the cache
    /// untouched, so card A's protocol bytes classified card B.
    #[test]
    fn on_off_on_card_swap_refreshes_cached_atr() {
        let atr_a = vec![0x3b, 0x00];
        let atr_b = vec![0x3b, 0x02, 0xff, 0x81];
        let driver = ScriptedDriver {
            response: Vec::new(),
            fail: false,
            session: true,
            last_apdu: Vec::new(),
            present: true,
            atrs: vec![atr_a.clone(), atr_b.clone()],
            presence_calls: 0,
            power_on_calls: 0,
        };
        let (mut handler, mut rx_buf) = harness(driver);
        let mut cached_atr = Vec::new();

        // ON with card A in the field primes the cache with A's ATR
        assert!(
            handle_message(&mut handler, &[VPCD_CTRL_ON], &mut rx_buf, &mut cached_atr).is_none()
        );
        assert_eq!(cached_atr, atr_a);

        // OFF invalidates — the card may be swapped out before the next ON
        assert!(
            handle_message(&mut handler, &[VPCD_CTRL_OFF], &mut rx_buf, &mut cached_atr).is_none()
        );
        assert!(cached_atr.is_empty());

        // ON again — the driver now returns card B's ATR
        assert!(
            handle_message(&mut handler, &[VPCD_CTRL_ON], &mut rx_buf, &mut cached_atr).is_none()
        );
        assert_eq!(cached_atr, atr_b);

        // ATR probe on the live session serves card B's ATR, never the
        // stale card-A one
        let resp = handle_message(&mut handler, &[VPCD_CTRL_ATR], &mut rx_buf, &mut cached_atr)
            .expect("ATR probe must answer");
        assert_eq!(resp, atr_b, "card swap must not serve card A's stale ATR");
    }

    /// #360 review (P2), RESET half: a successful RESET re-activation
    /// refreshes the cache; a failed one invalidates it instead of
    /// leaving the pre-RESET ATR armed.
    #[test]
    fn reset_refreshes_or_invalidates_cached_atr() {
        let atr_b = vec![0x3b, 0x02, 0xff];
        let driver = ScriptedDriver {
            response: Vec::new(),
            fail: false,
            session: false,
            last_apdu: Vec::new(),
            present: true,
            atrs: vec![atr_b.clone()],
            presence_calls: 0,
            power_on_calls: 0,
        };
        let (mut handler, mut rx_buf) = harness(driver);
        // stale card-A cache, as left by an earlier session
        let mut cached_atr = vec![0x3b, 0x00];

        let resp = handle_message(
            &mut handler,
            &[VPCD_CTRL_RESET],
            &mut rx_buf,
            &mut cached_atr,
        )
        .expect("RESET must answer");
        assert_eq!(resp, atr_b);
        assert_eq!(cached_atr, atr_b, "successful RESET must refresh the cache");

        // atrs now empty → the re-activation fails: ICC-absent + clear
        let resp = handle_message(
            &mut handler,
            &[VPCD_CTRL_RESET],
            &mut rx_buf,
            &mut cached_atr,
        )
        .expect("RESET must answer");
        assert!(resp.is_empty());
        assert!(
            cached_atr.is_empty(),
            "failed RESET must invalidate the stale cache"
        );
    }

    /// #360 review (P2), ON-failure half: a failed ON activation must
    /// clear the cache so a later ATR probe re-runs activation instead
    /// of serving card A's ATR for an empty field.
    #[test]
    fn failed_on_activation_invalidates_cached_atr() {
        let driver = ScriptedDriver {
            response: Vec::new(),
            fail: false,
            session: true,
            last_apdu: Vec::new(),
            present: true,
            atrs: Vec::new(), // power_on fails immediately
            presence_calls: 0,
            power_on_calls: 0,
        };
        let (mut handler, mut rx_buf) = harness(driver);
        // card A's ATR cached by an earlier successful session
        let mut cached_atr = vec![0x3b, 0x00];

        assert!(
            handle_message(&mut handler, &[VPCD_CTRL_ON], &mut rx_buf, &mut cached_atr).is_none()
        );
        assert!(
            cached_atr.is_empty(),
            "failed ON must invalidate the stale cache"
        );

        // the next probe re-runs the (still failing) activation rather
        // than answering from the stale cache
        let resp = handle_message(&mut handler, &[VPCD_CTRL_ATR], &mut rx_buf, &mut cached_atr)
            .expect("ATR probe must answer");
        assert!(
            resp.is_empty(),
            "probe after failed activation must answer ICC-absent, got {resp:?}"
        );
        assert_eq!(
            handler.driver_mut().power_on_calls,
            2,
            "probe must re-run activation (failed ON + probe)"
        );
    }
}
