//! Minimal TCP OTA flash server (port 3232): receives "OTA1" magic,
//! a u32-LE image size, then the raw app image bytes; writes them to
//! the inactive OTA partition via the esp-idf app_update API, switches
//! the boot partition, and reboots. Host client: ota_push.py.
//!
//! Authenticated form (build armed with `OTA_TOKEN=<32 chars>`): the
//! client sends "OTA2" + size + the 32-byte token instead — see
//! `ota_header` (pure logic + tests there). Unarmed builds keep the
//! original OTA1 behavior bit-for-bit.

use std::io::{Read, Write};
use std::net::TcpListener;

use esp_idf_sys::{
    esp_ota_abort, esp_ota_begin, esp_ota_end, esp_ota_get_next_update_partition,
    esp_ota_set_boot_partition, esp_ota_write, esp_restart,
};

use crate::ota_header::{self, MAGIC_V2, TOKEN_LEN};

use core::time::Duration;

const PORT: u16 = 3232;
const OTA_SIZE_UNKNOWN: usize = 0xFFFF_FFFF;
const CHUNK: usize = 1024;

/// Read the announced image size off the socket and write it to the
/// OTA partition. Fails (pre-commit) on any stalled read or write
/// reject — the caller aborts the handle (#223).
fn stream_image(
    stream: &mut impl Read,
    handle: esp_idf_sys::esp_ota_handle_t,
    size: usize,
) -> Result<(), &'static str> {
    let mut remaining = size;
    let mut buf = [0u8; CHUNK];
    while remaining > 0 {
        let want = remaining.min(CHUNK);
        stream
            .read_exact(&mut buf[..want])
            .map_err(|_| "image read failed")?;
        let rc = unsafe {
            esp_ota_write(
                handle,
                buf[..want].as_ptr() as *const core::ffi::c_void,
                want,
            )
        };
        if rc != 0 {
            return Err("esp_ota_write failed");
        }
        remaining -= want;
    }
    Ok(())
}

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
    log::warn!("ota: listening on :{}", PORT);
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                // #223: deadline every read/write on the connection — a
                // stalled client fails out instead of pinning this
                // accept thread forever.
                let io_timeout = Duration::from_millis(ota_header::OTA_IO_TIMEOUT_MS);
                let _ = stream.set_read_timeout(Some(io_timeout));
                let _ = stream.set_write_timeout(Some(io_timeout));
                if let Err(msg) = handle(&mut stream) {
                    log::warn!("ota: update failed: {}", msg);
                    let _ = stream.write_all(&[b'E']);
                }
            }
            Err(e) => log::warn!("ota: accept: {}", e),
        }
    }
}

fn handle(stream: &mut impl Read) -> Result<(), &'static str> {
    let mut hdr = [0u8; 8];
    stream
        .read_exact(&mut hdr)
        .map_err(|_| "header read failed")?;
    let magic: [u8; 4] = hdr[..4].try_into().expect("fixed slice");
    let size = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;

    // #223: size gate against the ACTUAL OTA partition (the old 4 MiB
    // constant was a proxy) — fetched before auth so oversize is
    // rejected pre-token, pre-erase.
    // Safety: read-only use of the partition-table-owned pointer.
    let part = unsafe { esp_ota_get_next_update_partition(core::ptr::null()) };
    if part.is_null() {
        return Err("no OTA partition — partition table lacks ota slots");
    }
    // Safety: aligned read of the fixed esp_partition_t layout.
    let partition_size = unsafe { (*part).size } as usize;
    ota_header::validate_size(size, partition_size)?;

    let expected = ota_header::compiled_token();
    if magic == MAGIC_V2 {
        let mut token = [0u8; TOKEN_LEN];
        stream
            .read_exact(&mut token)
            .map_err(|_| "token read failed")?;
        ota_header::authorize(&magic, Some(&token), expected.as_ref())?;
    } else {
        ota_header::authorize(&magic, None, expected.as_ref())?;
    }

    let mut handle: esp_idf_sys::esp_ota_handle_t = 0;
    let rc = unsafe { esp_ota_begin(part, OTA_SIZE_UNKNOWN, &mut handle) };
    if rc != 0 {
        return Err("esp_ota_begin failed");
    }

    // #223: every failure after esp_ota_begin aborts the OTA handle — a
    // leaked handle left the next attempt wedged until reboot.
    if let Err(msg) = stream_image(stream, handle, size) {
        // Safety: single-threaded app_update API use on our own handle.
        unsafe { esp_ota_abort(handle) };
        return Err(msg);
    }

    // Accounting invariant: stream_image only returns Ok when the full
    // announced size was written (remaining reaches 0 by construction —
    // the old post-loop `remaining != 0` check could never fire).
    // esp_ota_end is the commit gate: it validates image integrity
    // (checksum/signature) BEFORE the boot switch below; a failed end
    // finalizes the handle itself, so no abort on that path.
    let rc = unsafe { esp_ota_end(handle) };
    if rc != 0 {
        return Err("esp_ota_end failed — image rejected (bad checksum or size)");
    }
    let rc = unsafe { esp_ota_set_boot_partition(part) };
    if rc != 0 {
        return Err("set_boot_partition failed");
    }
    log::warn!(
        "ota: {} bytes written, boot partition switched, rebooting",
        size
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    unsafe { esp_restart() }
}
