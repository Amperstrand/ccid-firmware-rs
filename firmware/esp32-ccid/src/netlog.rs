//! UDP network logger: every log line goes to the console AND (once the
//! network is up) to a broadcast datagram on port 4567 — the bench log
//! channel when no serial link exists. Fire-and-forget: send failures
//! are swallowed so logging can never block the app.

use std::net::UdpSocket;
use std::sync::OnceLock;

use log::{LevelFilter, Log, Metadata, Record};

const LOG_PORT: u16 = 4567;

static SOCK: OnceLock<UdpSocket> = OnceLock::new();
static BOARD_IP: OnceLock<String> = OnceLock::new();

struct NetLogger;

impl Log for NetLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let ip = BOARD_IP.get().map(String::as_str).unwrap_or("no-ip");
        let line = format!(
            "[{} {} {}] {}",
            ip,
            record.level(),
            record.target(),
            record.args()
        );
        println!("{line}");
        if let Some(sock) = SOCK.get() {
            let _ = sock.send_to(line.as_bytes(), ("255.255.255.255", LOG_PORT));
        }
    }

    fn flush(&self) {}
}

/// Install the logger (console-only until `set_ip` opens the socket).
pub fn init() {
    if log::set_boxed_logger(Box::new(NetLogger)).is_ok() {
        log::set_max_level(LevelFilter::Warn);
    }
}

/// Record the station IP; it prefixes every subsequent log line so the
// host can learn the board's address from the log stream itself.
///
/// The UDP socket is only bound HERE, once the station has a lease:
/// binding before `esp_netif`/lwIP bring-up asserts inside lwIP
/// (`tcpip_send_msg_wait_sem: Invalid mbox`) and reboots the board.
/// Repeat calls keep the first socket (SOCK is a OnceLock and the
/// 0.0.0.0 bind survives IP changes); newer sockets are dropped.
pub fn set_ip(ip: &str) {
    let _ = BOARD_IP.set(ip.to_string());
    let sock = UdpSocket::bind("0.0.0.0:0").ok();
    let _ = sock.as_ref().map(|s| s.set_broadcast(true));
    if let Some(s) = sock {
        let _ = SOCK.set(s);
    }
}
