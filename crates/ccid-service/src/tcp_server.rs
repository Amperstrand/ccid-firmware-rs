//! Single-session TCP transport for the GemPC serial CCID server —
//! the `wifi-ccid` application path (t_72e1e2ed extraction).
//!
//! The firmware main (`firmware/esp32-ccid/src/wifi_ccid.rs`) used to
//! own this loop inline; it moves here so the #153 arrival-floor +
//! answer-or-silence and the idle-close tick architecture are
//! host-testable like the rest of the kernel. The loop body is
//! BYTE-IDENTICAL in behavior to the bench-hardened 2026-10-12 main
//! (verified loopback via socat PTY↔TCP + stock libccidtwin): every
//! read is a 250 ms tick, so RF presence polling keeps running when
//! the host goes quiet, bytes queued behind a blocking handler are
//! anchored at the burst's ARRIVAL floor, responses that outlived the
//! host's read window are dropped (`record_late_response`) instead of
//! poisoning the stream, and a host silent for 10 min is dropped.
//!
//! # Boundary (the mute-recovery seam)
//!
//! The original main recovered a mute PN7160 in-loop: it read the
//! I²C send-failure streak and cycled VEN on the concrete transport —
//! both board-crate internals this kernel must not see. The seam is
//! an injected [`on_tick`] callback: the kernel calls it once per
//! serve-loop tick (after the read handling, before the presence
//! poll — the exact position the inline ladder occupied) with the
//! live card driver; the firmware side owns the streak check, the
//! VEN power cycle and the init-ladder replay. Tick cadence is the
//! recovery cadence: the callback fires on every non-terminating
//! iteration, exactly where the bench-verified ladder ran.
//!
//! Board specifics that stay with the caller: the session-fd
//! publication for the #156 B stall-hook writer, the accept/reject
//! outer loop's delays, and the clock (FreeRTOS
//! `xTaskGetTickCount()` on target, a scripted counter in tests).

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use crate::ccid_serial_server::{CcidSerialServer, ServeAction};
use crate::nfc::NfcDriver;

/// One read tick: 250 ms read timeout makes every loop iteration a
/// tick (bench hardening 2026-10-12), so RF presence polling runs on
/// the poll interval even when the host goes quiet (a blocking read
/// froze polling through pcscd restarts).
pub const READ_TICK: Duration = Duration::from_millis(250);
/// 250 ms read ticks × 2400 = 10 min without host bytes → drop client.
pub const IDLE_CLOSE_TICKS: u32 = 2400;
/// Burst buffer: one read = one burst — every byte in it shares the
/// arrival floor captured before it. A full read may leave older
/// queued bytes, so the floor advances only on a SHORT read.
const BURST_BUF_LEN: usize = 64;

/// Tunables a caller may shrink for tests; defaults are the
/// production wifi-ccid values above.
#[derive(Debug, Clone, Copy)]
pub struct SessionConfig {
    /// Per-read timeout = one tick.
    pub read_timeout: Duration,
    /// Timed-out reads before an idle close.
    pub idle_close_ticks: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            read_timeout: READ_TICK,
            idle_close_ticks: IDLE_CLOSE_TICKS,
        }
    }
}

/// Per-session static context the loop needs beyond its handles.
#[derive(Debug, Clone, Copy)]
pub struct SessionParams<'a> {
    /// Tick/idle tunables — production defaults in
    /// [`SessionConfig::default`].
    pub cfg: SessionConfig,
    /// Log prefix for every session log line (e.g. `"wifi-ccid"`), so
    /// the extracted loop keeps the main's exact bench-log lines.
    pub log_tag: &'a str,
    /// Peer address, for the same log lines.
    pub peer: SocketAddr,
}

/// Why a session ended. Every terminating path logs before returning
/// (same lines the inline loop emitted), so the caller only needs to
/// retire its own session state (e.g. the stall-hook fd).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// Peer closed the stream (read returned EOF).
    ClientClosed,
    /// [`SessionConfig::idle_close_ticks`] timed-out reads in a row.
    IdleTimeout,
    /// Hard read I/O error — the client is dropped.
    ReadError,
}

/// Serve ONE accepted TCP client: the tick loop, byte-for-byte the
/// 2026-10-12 wifi-ccid session loop (#153 semantics locked by the
/// host tests below).
///
/// * `stream` — the accepted session stream (`Read + Write`, so host
///   tests can drive it with a scripted duplex — the vpcd-suite
///   pattern); the TcpStream entry point is [`serve_tcp_session`].
/// * `listener` — the SAME listener the caller accepts sessions on;
///   it must be NONBLOCKING. Each tick drains one queued extra client
///   and rejects it (single-session contract: queued clients used to
///   sit in the kernel backlog as ESTAB-looking zombies).
/// * `now` — tick source (FreeRTOS `xTaskGetTickCount()` on target).
/// * `on_tick` — the mute-recovery seam: called once per
///   non-terminating tick with the live driver (see module docs).
///
/// The listener is a `std::net` type on every target the kernel
/// serves from (lwIP via esp-idf's std), and host tests bind a real
/// loopback listener for the extra-client rejection contract.
pub fn serve_session<S, D>(
    stream: &mut S,
    listener: &TcpListener,
    server: &mut CcidSerialServer<D>,
    params: &SessionParams<'_>,
    now: &mut dyn FnMut() -> u32,
    on_tick: &mut dyn FnMut(&mut D),
) -> SessionEnd
where
    S: Read + Write,
    D: NfcDriver,
{
    let SessionParams { cfg, log_tag, peer } = *params;
    let mut burst_buf = [0u8; BURST_BUF_LEN];
    let mut idle_ticks: u32 = 0;
    // #153 arrival floor (Codex round 2 on #158): bytes queued in
    // the TCP stack during a blocking handler are returned by the
    // NEXT read — anchor them at the tick of the last dispatch,
    // never at the drain-time read. A timed-out read proves the
    // stream idle and raises the floor.
    let mut arrival_floor = now();
    loop {
        match stream.read(&mut burst_buf) {
            Ok(0) => {
                log::warn!("{}: client {} closed", log_tag, peer);
                return SessionEnd::ClientClosed;
            }
            Ok(n) => {
                idle_ticks = 0;
                // One read = one burst: every byte in it shares the
                // floor captured before it.
                let read_end = now();
                let floor = arrival_floor;
                for &byte in &burst_buf[..n] {
                    let tick = now();
                    if server.feed_byte(byte, tick) == ServeAction::Respond {
                        // #153: the frame may have queued behind the
                        // previous handler — anchor it at the burst's
                        // arrival floor before judging expiry, then drop
                        // responses that outlived the host's read window
                        // instead of poisoning the stream.
                        server.refresh_host_deadline(floor);
                        let post = now();
                        if server.response_expired(post) {
                            server.record_late_response();
                            continue;
                        }
                        let echo = server.echo();
                        let resp = server.response();
                        if stream.write_all(echo).is_err() || stream.write_all(resp).is_err() {
                            // Burst abandoned, session state intact: the
                            // next read fails (broken pipe) and drops the
                            // client — the inline loop's `break` semantics.
                            break;
                        }
                        // Dump-and-retrieve: escape 0xD1 ack went out —
                        // panic now so the flash coredump captures state.
                        if server.take_snapshot_request() {
                            panic!("escape 0xD1: diagnostic snapshot requested");
                        }
                    }
                }
                // Codex round 3 on #168: advance the floor only on a
                // SHORT read (queue plausibly drained at read
                // return) — a full read may leave older queued bytes.
                if n < burst_buf.len() {
                    arrival_floor = read_end;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                idle_ticks += 1;
                if idle_ticks >= cfg.idle_close_ticks {
                    log::warn!("{}: client {} idle timeout — closing", log_tag, peer);
                    return SessionEnd::IdleTimeout;
                }
                // The timed-out read proves nothing queued: later
                // bytes are fresh arrivals.
                arrival_floor = now();
            }
            Err(e) => {
                log::warn!("{}: read error ({:?}) — dropping client", log_tag, e);
                return SessionEnd::ReadError;
            }
        }
        // Single-session contract: reject queued extra clients now,
        // not when the current session ends.
        if let Ok((extra, extra_peer)) = listener.accept() {
            let _ = extra.shutdown(Shutdown::Both);
            log::warn!("{}: rejected extra client {}", log_tag, extra_peer);
        }
        // Mute recovery (two faces, bench 2026-10-12): a NAKing PN7160
        // (send streak) or a semi-muted one that ACKs writes but never
        // answers RSPs (silent streak) — the board's on-tick hook runs
        // the VEN-cycle + ladder replay in the exact position the
        // inline ladder occupied.
        on_tick(server.handler_mut().driver_mut());
        let tick = now();
        server.poll_if_due(tick);
    }
}

/// TcpStream entry point: applies the tick read-timeout (and makes
/// the rejection listener nonblocking — both idempotent if, like the
/// wifi-ccid main, the caller already configured them) and enters
/// [`serve_session`].
pub fn serve_tcp_session<D: NfcDriver>(
    stream: &mut TcpStream,
    listener: &TcpListener,
    server: &mut CcidSerialServer<D>,
    params: &SessionParams<'_>,
    now: &mut dyn FnMut() -> u32,
    on_tick: &mut dyn FnMut(&mut D),
) -> SessionEnd {
    let _ = stream.set_read_timeout(Some(params.cfg.read_timeout));
    let _ = listener.set_nonblocking(true);
    serve_session(stream, listener, server, params, now, on_tick)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccid_handler::CcidHandler;
    use crate::nfc::MockNfcDriver;
    use ccid_protocol::types::PC_TO_RDR_GET_SLOT_STATUS;
    use ccid_transport_serial::{calculate_lrc, CTRL_ACK, SYNC};
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;

    /// Poll interval far beyond every scripted tick: presence polls
    /// (and their mock-driver bookkeeping) stay out of the assertions.
    const POLL_INTERVAL: u32 = 1_000_000;
    /// Tests model the 1 kHz tick rate like the ccid_serial_server
    /// suite (ticks == ms): tight #153 window = 250 ticks.
    const TEST_WINDOWS: crate::ccid_serial_server::ResponseWindows =
        crate::ccid_serial_server::ResponseWindows::from_tick_hz(1_000);

    /// Scripted duplex (the vpcd-suite pattern, TCP-shaped): successive
    /// reads pop successive script chunks; drained script reads EOF
    /// (`eof`) or a WouldBlock tick (the timeout face), and every write
    /// lands in `wrote` for byte-exact assertions.
    struct ScriptedStream {
        script: Vec<Vec<u8>>,
        next: usize,
        eof_when_drained: bool,
        wrote: Vec<u8>,
    }

    impl ScriptedStream {
        fn bursts(bursts: &[&[u8]]) -> Self {
            Self {
                script: bursts.iter().map(|b| b.to_vec()).collect(),
                next: 0,
                eof_when_drained: true,
                wrote: Vec::new(),
            }
        }

        fn idle() -> Self {
            Self {
                script: Vec::new(),
                next: 0,
                eof_when_drained: false,
                wrote: Vec::new(),
            }
        }
    }

    impl Read for ScriptedStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.next < self.script.len() {
                let chunk = &self.script[self.next];
                self.next += 1;
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                Ok(n)
            } else if self.eof_when_drained {
                Ok(0)
            } else {
                // A read that times out with nothing queued = one tick.
                Err(io::Error::new(io::ErrorKind::WouldBlock, "scripted tick"))
            }
        }
    }

    impl Write for ScriptedStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.wrote.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Deterministic tick source: +1 per call (ticks == ms).
    struct SteppedClock(u32);
    impl SteppedClock {
        fn tick(&mut self) -> u32 {
            self.0 += 1;
            self.0
        }
    }

    /// Scripted tick source for the #153 verdict test: pops scripted
    /// values, then repeats the last.
    struct ScriptedClock {
        ticks: Vec<u32>,
        call: usize,
    }
    impl ScriptedClock {
        fn tick(&mut self) -> u32 {
            let i = self.call.min(self.ticks.len() - 1);
            self.call += 1;
            self.ticks[i]
        }
    }

    /// Host→reader GetSlotStatus frame (no payload): SYNC + ACK +
    /// 10-byte CCID header + LRC — same builder shape as the
    /// ccid_serial_server suite.
    fn slot_status_frame(seq: u8) -> [u8; 13] {
        let ccid = [PC_TO_RDR_GET_SLOT_STATUS, 0, 0, 0, 0, 0, seq, 0, 0, 0];
        let mut frame = [0u8; 13];
        frame[0] = SYNC;
        frame[1] = CTRL_ACK;
        frame[2..12].copy_from_slice(&ccid);
        frame[12] = calculate_lrc(&frame[..12]);
        frame
    }

    fn server_no_card() -> CcidSerialServer<MockNfcDriver> {
        let mut driver = MockNfcDriver::new(false, &[0x3B, 0x80, 0x01, 0x01], &[0x90, 0x00]);
        driver.init().unwrap();
        CcidSerialServer::new(CcidHandler::new(driver), POLL_INTERVAL, TEST_WINDOWS, 0)
    }

    /// Loopback listener with nothing queued: `accept()` WouldBlocks,
    /// the rejection branch stays cold.
    fn quiet_listener() -> TcpListener {
        let l = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        l.set_nonblocking(true).expect("nonblocking");
        l
    }

    /// One burst in, echo+response out, EOF ends the session — the
    /// round-trip the socat/libccidtwin loopback proved on hardware.
    #[test]
    fn burst_answers_echo_then_response_and_closes_on_eof() {
        let mut server = server_no_card();
        let mut stream = ScriptedStream::bursts(&[&slot_status_frame(7)]);
        let mut clock = SteppedClock(0);
        let mut ticks = 0usize;
        let mut on_tick = |_: &mut MockNfcDriver| ticks += 1;
        let end = serve_session(
            &mut stream,
            &quiet_listener(),
            &mut server,
            &SessionParams {
                cfg: SessionConfig::default(),
                log_tag: "wifi-ccid",
                peer: "127.0.0.1:1".parse().unwrap(),
            },
            &mut || clock.tick(),
            &mut on_tick,
        );

        assert_eq!(end, SessionEnd::ClientClosed);
        let frame = slot_status_frame(7);
        assert_eq!(
            &stream.wrote[..frame.len()],
            &frame,
            "echo is the frame verbatim"
        );
        let resp = server.response();
        assert_eq!(
            &stream.wrote[frame.len()..],
            resp,
            "framed response follows the echo"
        );
        assert_eq!(resp[0], SYNC);
        assert_eq!(resp[1], CTRL_ACK);
        assert_eq!(resp[2], 0x81, "RDR_to_PC_SlotStatus");
        assert_eq!(resp[8], 7, "bSeq echoed");
        assert_eq!(server.late_drop_count(), 0);
        assert!(ticks >= 1, "the on-tick recovery seam must fire per tick");
    }

    /// Idle-close: nothing to read, every read is a timed-out tick —
    /// the session drops after `idle_close_ticks` ticks, and the
    /// recovery seam fires on every NON-terminating tick (the closing
    /// tick returns before the tail), the exact cadence the inline
    /// PN7160 ladder relied on.
    #[test]
    fn idle_ticks_close_session_and_fire_the_recovery_seam_per_tick() {
        let mut server = server_no_card();
        let mut stream = ScriptedStream::idle();
        let mut clock = SteppedClock(0);
        let cfg = SessionConfig {
            read_timeout: Duration::from_millis(1),
            idle_close_ticks: 3,
        };
        let ticks = Arc::new(AtomicUsize::new(0));
        let seam_calls = Arc::clone(&ticks);
        let mut on_tick = move |_: &mut MockNfcDriver| {
            seam_calls.fetch_add(1, Ordering::SeqCst);
        };
        let end = serve_session(
            &mut stream,
            &quiet_listener(),
            &mut server,
            &SessionParams {
                cfg,
                log_tag: "wifi-ccid",
                peer: "127.0.0.1:1".parse().unwrap(),
            },
            &mut || clock.tick(),
            &mut on_tick,
        );

        assert_eq!(end, SessionEnd::IdleTimeout);
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            2,
            "seam fires once per non-terminating tick; the closing tick returns first"
        );
        assert!(stream.wrote.is_empty(), "no responses while idle");
        assert_eq!(server.late_drop_count(), 0);
    }

    /// #153 answer-or-silence through the TCP loop: a response whose
    /// verdict tick (`post`) lands past the arrival-anchored window is
    /// DROPPED — nothing goes on the wire — and the drop is counted.
    /// The clock scripts the jump the blocking handler would have
    /// caused in real time.
    #[test]
    fn response_past_the_arrival_anchored_window_is_dropped_not_written() {
        let mut server = server_no_card();
        let frame = slot_status_frame(3);
        let mut stream = ScriptedStream::bursts(&[&frame]);
        // Clock calls, in order: floor init, read_end, 13 per-byte
        // stamps, then the verdict stamp (`post`) far past the 250-tick
        // tight window — the jump a blocking handler causes in real
        // time — and the trailing poll stamp.
        let mut clock = ScriptedClock {
            ticks: vec![0, 0],
            call: 0,
        };
        clock.ticks.extend(1..=14u32); // 13 byte stamps + `post`
        clock.ticks[15] = 100_000; // `post` — handler blew the window
        clock.ticks.push(100_001); // poll_if_due stamp
        let mut on_tick = |_: &mut MockNfcDriver| {};
        let end = serve_session(
            &mut stream,
            &quiet_listener(),
            &mut server,
            &SessionParams {
                cfg: SessionConfig::default(),
                log_tag: "wifi-ccid",
                peer: "127.0.0.1:1".parse().unwrap(),
            },
            &mut || clock.tick(),
            &mut on_tick,
        );

        assert_eq!(end, SessionEnd::ClientClosed);
        assert!(
            stream.wrote.is_empty(),
            "a late response must never reach the wire (libccidtwin cannot resync): {:?}",
            stream.wrote
        );
        assert_eq!(server.late_drop_count(), 1);
    }

    /// Single-session contract: a client queued on the listener DURING
    /// a live session is accepted-and-rejected on the very next tick —
    /// never left as an ESTAB-looking zombie in the backlog.
    #[test]
    fn queued_extra_client_is_rejected_within_the_tick() {
        let listener = quiet_listener();
        // Real queued connection on the real loopback listener.
        let mut extra = TcpStream::connect(listener.local_addr().unwrap()).expect("extra connects");
        // Backlog the queue before the serve loop runs its accept.
        let frame = slot_status_frame(9);
        let mut server = server_no_card();
        let mut stream = ScriptedStream::bursts(&[&frame]);
        let mut clock = SteppedClock(0);
        let mut on_tick = |_: &mut MockNfcDriver| {};
        let end = serve_session(
            &mut stream,
            &listener,
            &mut server,
            &SessionParams {
                cfg: SessionConfig::default(),
                log_tag: "wifi-ccid",
                peer: "127.0.0.1:1".parse().unwrap(),
            },
            &mut || clock.tick(),
            &mut on_tick,
        );
        assert_eq!(end, SessionEnd::ClientClosed);

        // The extra client was shutdown(Both): its next read sees EOF
        // (or a reset — both mean "no session for you").
        extra
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut scratch = [0u8; 1];
        let verdict = extra.read(&mut scratch);
        assert!(
            matches!(verdict, Ok(0))
                || matches!(verdict, Err(ref e) if e.kind() == io::ErrorKind::ConnectionReset),
            "extra client must be rejected, got {verdict:?}"
        );
        // And the session itself completed normally despite the reject.
        assert!(!stream.wrote.is_empty());
    }

    /// The TcpStream entry point over a REAL loopback socket pair —
    /// the firmware wiring (timeout + nonblocking applied by
    /// `serve_tcp_session`) driving a scripted host client thread.
    #[test]
    fn tcp_stream_entry_serves_a_real_socket_pair() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel::<SessionEnd>();

        let frame = slot_status_frame(5);
        let join = std::thread::spawn(move || {
            // Scripted host client: send the frame, read exactly
            // echo (13 B) + SlotStatus response (13 B), then close.
            let mut host = TcpStream::connect(addr).expect("client connect");
            host.write_all(&frame).expect("send frame");
            let mut got = [0u8; 26];
            host.read_exact(&mut got).expect("read echo+response");
            assert_eq!(&got[..13], &frame, "echo first, verbatim");
            assert_eq!(got[15], 0x81, "framed SlotStatus follows");
            drop(host); // EOF → ClientClosed on the serve side
            tx.send(SessionEnd::ClientClosed).ok();
        });

        let (mut session, peer) = listener.accept().expect("accept session");
        let _ = session.set_nodelay(true);
        let mut server = server_no_card();
        let mut clock = SteppedClock(0);
        let mut on_tick = |_: &mut MockNfcDriver| {};
        let cfg = SessionConfig {
            read_timeout: Duration::from_millis(50),
            idle_close_ticks: 1000,
        };
        let end = serve_tcp_session(
            &mut session,
            &listener,
            &mut server,
            &SessionParams {
                cfg,
                log_tag: "wifi-ccid",
                peer,
            },
            &mut || clock.tick(),
            &mut on_tick,
        );

        assert_eq!(
            end,
            rx.recv_timeout(Duration::from_secs(5))
                .expect("client finished")
        );
        join.join().expect("client thread");
    }
}
