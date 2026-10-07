//! Minimal TCP OTA flash server (port 3232): receives "OTA1" magic,
//! a u32-LE image size, then the raw app image bytes; writes them to
//! the inactive OTA partition via the esp-idf app_update API, switches
//! the boot partition, and reboots. Host client: ota_push.py.
//!
//! **BENCH/DEBUG ONLY** — gated behind the `bench-net` feature. The
//! protocol carries NO authentication: anyone who can reach the board
//! on the bench LAN can replace its firmware. Signed/authenticated OTA
//! is tracked in a follow-up issue; until then this module must never
//! be compiled into a production artifact.
//!
//! Hardening invariants (keep when extending):
//! - socket read/write deadlines on every connection — a stalled client
//!   cannot hold the OTA thread indefinitely
//! - the announced image size is validated against the ACTUAL target OTA
//!   partition size before any erase/write begins
//! - every failure after a successful `esp_ota_begin` calls
//!   `esp_ota_abort` (except after `esp_ota_end`, which deactivates its
//!   own handle on validation failure)
//! - the boot partition is switched ONLY after `esp_ota_end` accepted
//!   the image (esp-idf validates the app image header + checksum there)

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use esp_idf_sys::{
    esp_ota_abort, esp_ota_begin, esp_ota_end, esp_ota_get_next_update_partition,
    esp_ota_set_boot_partition, esp_ota_write, esp_restart,
};

const PORT: u16 = 3232;
const OTA_SIZE_UNKNOWN: usize = 0xFFFF_FFFF;
const MAGIC: [u8; 4] = *b"OTA1";
const CHUNK: usize = 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn spawn() {
    let _ = std::thread::Builder::new()
        .stack_size(16 * 1024)
        .name("ota".into())
        .spawn(server);
}

fn server() {
    let listener = match TcpListener::bind(("0.0.0.0", PORT)) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("ota: bind failed: {}", e);
            return;
        }
    };
    log::warn!("ota: listening on :{} (UNAUTHENTICATED bench build)", PORT);
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Err(msg) = handle(&mut stream) {
                    log::warn!("ota: update failed: {}", msg);
                    let _ = stream.write_all(&[b'E']);
                }
            }
            Err(e) => log::warn!("ota: accept: {}", e),
        }
    }
}

fn handle(stream: &mut TcpStream) -> Result<(), &'static str> {
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|_| "set read timeout failed")?;
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .map_err(|_| "set write timeout failed")?;

    let mut hdr = [0u8; 8];
    stream
        .read_exact(&mut hdr)
        .map_err(|_| "header read failed")?;
    if hdr[..4] != MAGIC {
        return Err("bad magic");
    }
    let size = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
    if size == 0 {
        return Err("bad size");
    }

    // Safety: single-threaded use of the app_update API on a partition
    // pointer owned by the esp-idf partition table.
    let part = unsafe { esp_ota_get_next_update_partition(core::ptr::null()) };
    if part.is_null() {
        return Err("no OTA partition — partition table lacks ota slots");
    }
    // Bound the announced size by the actual target partition — NOT a
    // generic flash-size guess. Rejects oversize images before any
    // erase/write happens.
    let part_size = unsafe { (*part).size as usize };
    if size > part_size {
        return Err("image larger than OTA partition");
    }

    let mut handle: esp_idf_sys::esp_ota_handle_t = 0;
    let rc = unsafe { esp_ota_begin(part, OTA_SIZE_UNKNOWN, &mut handle) };
    if rc != 0 {
        return Err("esp_ota_begin failed");
    }

    let mut remaining = size;
    let mut buf = [0u8; CHUNK];
    while remaining > 0 {
        let want = remaining.min(CHUNK);
        if stream.read_exact(&mut buf[..want]).is_err() {
            unsafe { esp_ota_abort(handle) };
            return Err("image read failed");
        }
        let rc = unsafe {
            esp_ota_write(
                handle,
                buf[..want].as_ptr() as *const core::ffi::c_void,
                want,
            )
        };
        if rc != 0 {
            unsafe { esp_ota_abort(handle) };
            return Err("esp_ota_write failed");
        }
        remaining -= want;
    }

    // esp_ota_end validates the written app image (header + checksum)
    // and deactivates the handle itself on failure — no abort here.
    let rc = unsafe { esp_ota_end(handle) };
    if rc != 0 {
        return Err("esp_ota_end failed (invalid image)");
    }
    let rc = unsafe { esp_ota_set_boot_partition(part) };
    if rc != 0 {
        return Err("set_boot_partition failed");
    }
    log::warn!(
        "ota: {} bytes written, boot partition switched, rebooting",
        size
    );
    std::thread::sleep(Duration::from_millis(500));
    unsafe { esp_restart() }
}
