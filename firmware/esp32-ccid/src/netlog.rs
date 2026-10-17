//! Network-aware logger: every log line goes to the console AND, in
//! `bench-net` builds once the network is up, to a broadcast datagram
//! on port 4567 — the bench log channel when no serial link exists.
//! The UDP arm is compiled ONLY under `bench-net` (#219): shipped
//! builds get the console logger and no network surface at all.
//! Fire-and-forget: send failures are swallowed so logging can never
//! block the app.

use std::sync::OnceLock;

use log::{LevelFilter, Log, Metadata, Record};

static BOARD_IP: OnceLock<String> = OnceLock::new();

#[cfg(feature = "bench-net")]
mod udp {
    use std::net::UdpSocket;
    use std::sync::OnceLock;

    pub(super) const LOG_PORT: u16 = 4567;
    pub(super) static SOCK: OnceLock<UdpSocket> = OnceLock::new();
    pub(super) static BROADCAST_ADDR: OnceLock<String> = OnceLock::new();
}

#[cfg(feature = "bench-net")]
use std::net::UdpSocket;

#[cfg(feature = "bench-net")]
use udp::{BROADCAST_ADDR, LOG_PORT, SOCK};

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
        #[cfg(feature = "bench-net")]
        if let Some(sock) = SOCK.get() {
            let dst = BROADCAST_ADDR
                .get()
                .map(String::as_str)
                .unwrap_or("255.255.255.255");
            let _ = sock.send_to(line.as_bytes(), (dst, LOG_PORT));
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

// bench-net builds only (see module docs).
/// Record the station IP; it prefixes every subsequent log line so the
/// host can learn the board's address from the log stream itself.
///
/// The UDP socket is only bound HERE, once the station has a lease:
/// binding before `esp_netif`/lwIP bring-up asserts inside lwIP
/// (`tcpip_send_msg_wait_sem: Invalid mbox`) and reboots the board.
/// Repeat calls keep the first socket (SOCK is a OnceLock and the
/// 0.0.0.0 bind survives IP changes); newer sockets are dropped.
#[cfg(feature = "bench-net")]
pub fn set_ip(ip: &str) {
    let _ = BOARD_IP.set(ip.to_string());
    // Subnet-directed broadcast, not limited broadcast: the bench AP does
    // not forward 255.255.255.255 between wireless stations (tcpdump-
    // verified: datagrams left the board, never arrived at the host).
    // /24 bench subnet — keep the first three octets, broadcast in the last.
    if let Some((prefix, _)) = ip.rsplit_once('.') {
        let _ = BROADCAST_ADDR.set(format!("{prefix}.255"));
    }
    let sock = UdpSocket::bind("0.0.0.0:0").ok();
    let _ = sock.as_ref().map(|s| s.set_broadcast(true));
    if let Some(s) = sock {
        let _ = SOCK.set(s);
    }
}
