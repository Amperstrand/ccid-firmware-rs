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
use std::sync::Mutex;
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
                continue
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

/// What the absence gate decided to log for one absent ATR poll.
/// Returned by [`note_absent_poll`] so tests observe the exact decision.
#[derive(Debug, PartialEq, Eq)]
enum AbsenceNote {
    /// present→absent transition (or first absence from the initial
    /// unannounced state) — announce once.
    Transition,
    /// Still absent — slow heartbeat; payload is the poll count.
    Heartbeat(u32),
}

/// #332/#350: ONE shared absence-transition gate for the ATR path.
///
/// Codex P1 on #350: the previous code kept two function-local
/// `WAS_PRESENT` statics (one per note fn) and announced when the swap
/// returned *absent*, so (a) on every consecutive absent poll the flag
/// was already `false` and the warning repeated each ~400 ms probe
/// instead of decaying to a ~6 min heartbeat, and (b) a successful ATR
/// wrote a static the absent path never read, so presence could not
/// re-arm the announcement. This struct is the single state both paths
/// share; [`ABSENCE_GATE`] below is the live instance.
///
/// Transition-gated semantics:
/// * absent after present (or after boot — the initial state counts as
///   unannounced) → announce once;
/// * absent after absent → silent, except a heartbeat every
///   [`AbsenceGate::ABSENCE_LOG_EVERY`] polls;
/// * present → silent, and re-arms the next transition.
#[derive(Debug)]
struct AbsenceGate {
    /// Verdict of the last poll seen by EITHER path. Starts `true` so
    /// the first absence at boot announces (the initial-unannounced
    /// state behaves as a transition, per the #350 review).
    was_present: bool,
    /// Consecutive absent polls since the last announcement (wraps like
    /// the old fetch_add counter; presence resets it).
    absent_polls: u32,
}

impl AbsenceGate {
    /// ~6 min at pcscd's ~400 ms ATR probe — the absence heartbeat.
    const ABSENCE_LOG_EVERY: u32 = 900;

    const fn new() -> Self {
        Self {
            was_present: true,
            absent_polls: 0,
        }
    }

    /// Absent ATR verdict: `Some(note)` when a warning should be logged.
    fn note_absent(&mut self) -> Option<AbsenceNote> {
        self.absent_polls = self.absent_polls.wrapping_add(1);
        if self.was_present {
            self.was_present = false;
            self.absent_polls = 1;
            Some(AbsenceNote::Transition)
        } else if self.absent_polls.is_multiple_of(Self::ABSENCE_LOG_EVERY) {
            Some(AbsenceNote::Heartbeat(self.absent_polls))
        } else {
            None
        }
    }

    /// Present ATR verdict (successful power_on): silent, re-arms the
    /// present→absent announcement and resets the heartbeat counter.
    fn note_present(&mut self) {
        self.was_present = true;
        self.absent_polls = 0;
    }
}

impl Default for AbsenceGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Live gate shared by both ATR paths — the single state the #350 P1
/// asked for. Mutex only because a static must be `Sync`; the vpcd
/// client is one thread.
static ABSENCE_GATE: Mutex<AbsenceGate> = Mutex::new(AbsenceGate::new());

fn note_absent_poll() -> Option<AbsenceNote> {
    let mut gate = ABSENCE_GATE.lock().unwrap_or_else(|p| p.into_inner());
    let note = gate.note_absent();
    match note {
        Some(AbsenceNote::Transition) => {
            log::warn!("vpcd: ATR req → NOT present → empty (driver re-polls)");
        }
        Some(AbsenceNote::Heartbeat(n)) => log::warn!("vpcd: still absent ({} polls)", n),
        None => {}
    }
    note
}

fn note_present_poll() {
    ABSENCE_GATE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .note_present();
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
                            note_present_poll();
                            Some(atr[..len].to_vec())
                        }
                        Err(_) => {
                            log::warn!("vpcd: ATR req → present, power_on FAILED → empty");
                            Some(Vec::new()) // power failed → "no card"
                        }
                    }
                } else {
                    // Codex P1 on #328 (tracked #332): pcscd probes ATR
                    // presence at ~400 ms with no card — logging EVERY
                    // absent answer spams the console indefinitely.
                    // Log the ABSENCE TRANSITION only (first absent
                    // after presence, or every ABSENCE_LOG_EVERY as a
                    // slow heartbeat).
                    note_absent_poll();
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

    /// #350 regression (Codex P1), part 1: N consecutive absent polls
    /// must produce exactly ONE announcement plus the slow heartbeat.
    /// The old swap-based gate saw `was_present == false` on every
    /// consecutive absent poll and re-announced each ~400 ms probe.
    #[test]
    fn absent_streak_announces_once_then_heartbeats() {
        let mut gate = AbsenceGate::new();
        // First absence (the initial-unannounced state counts as a
        // transition): announced exactly once.
        assert_eq!(gate.note_absent(), Some(AbsenceNote::Transition));
        let polls = 2 * AbsenceGate::ABSENCE_LOG_EVERY; // ~12 min of absence
        let mut heartbeats = 0;
        let mut silent = 0;
        for _ in 2..=polls {
            match gate.note_absent() {
                Some(AbsenceNote::Heartbeat(n)) => {
                    heartbeats += 1;
                    assert_eq!(
                        n,
                        heartbeats * AbsenceGate::ABSENCE_LOG_EVERY,
                        "heartbeat must fire on the exact poll multiple"
                    );
                }
                Some(AbsenceNote::Transition) => panic!(
                    "absent-after-absent must not re-announce — per-poll \
                     spam is the #350 regression"
                ),
                None => silent += 1,
            }
        }
        assert_eq!(heartbeats, 2, "one heartbeat per ~6 min of absence");
        assert_eq!(silent, polls - 3, "every other absent poll is silent");
    }

    /// #350 regression (Codex P1), part 2: a present poll between
    /// absences re-arms the transition. The old present path wrote a
    /// different function-local static, so the gate never saw it and a
    /// successful ATR could not close the absence announcement.
    #[test]
    fn present_poll_rearms_the_absence_transition() {
        let mut gate = AbsenceGate::new();
        assert_eq!(gate.note_absent(), Some(AbsenceNote::Transition));
        assert_eq!(gate.note_absent(), None);
        gate.note_present(); // successful ATR between absences
        assert_eq!(
            gate.note_absent(),
            Some(AbsenceNote::Transition),
            "present→absent after a successful ATR must announce again"
        );
        assert_eq!(gate.note_absent(), None);
        // The re-arm also resets the heartbeat cadence: the next
        // heartbeat is a full ABSENCE_LOG_EVERY absent polls after the
        // re-armed transition, not sooner (2 absent polls counted
        // above).
        for _ in 3..AbsenceGate::ABSENCE_LOG_EVERY {
            assert_eq!(gate.note_absent(), None);
        }
        assert_eq!(
            gate.note_absent(),
            Some(AbsenceNote::Heartbeat(AbsenceGate::ABSENCE_LOG_EVERY))
        );
    }

    /// #350 wiring regression: both `note_*_poll` fns must drive the
    /// ONE shared `ABSENCE_GATE`. This is the only test touching the
    /// process-wide statics, so it cannot race the gate-local tests
    /// above under the parallel test harness.
    #[test]
    fn note_poll_fns_share_one_gate() {
        {
            let mut gate = ABSENCE_GATE.lock().unwrap_or_else(|p| p.into_inner());
            *gate = AbsenceGate::new();
        }
        assert_eq!(note_absent_poll(), Some(AbsenceNote::Transition));
        assert_eq!(
            note_absent_poll(),
            None,
            "consecutive absent polls must stay silent"
        );
        note_present_poll(); // the call the split-statics bug hid from the gate
        assert_eq!(
            note_absent_poll(),
            Some(AbsenceNote::Transition),
            "a successful ATR must close and re-arm the SHARED gate"
        );
        assert_eq!(note_absent_poll(), None);
    }
}
