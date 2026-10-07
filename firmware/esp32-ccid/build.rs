use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    embuild::espidf::sysenv::output();

    // WiFi credentials are baked via option_env! in the crate sources —
    // cargo must treat them as compilation inputs. Without this, changing
    // NUCULA_WIFI_SSID/PASS and running `cargo build` silently reuses the
    // cached rlib with the OLD credentials (the bench flash stale-image
    // bug: three consecutive "successful" flashes wrote the wrong SSID).
    println!("cargo:rerun-if-env-changed=NUCULA_WIFI_SSID");
    println!("cargo:rerun-if-env-changed=NUCULA_WIFI_PASS");

    // Firmware identity: every binary logs `FWID <name> rev=<git> build=<ts>`
    // as its first console line, so the HIL framework can PROVE which
    // firmware booted after a flash. Root cause: esptool's RTS hard-reset
    // on the C3 USB-JTAG is intermittently dropped, leaving the OLD
    // firmware running after a "successful" flash — tests then silently
    // exercise stale code (caught on the bench 2026-10-07: health counter
    // continued 52→124 across a flash).
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    // Best-effort: re-stamp when the commit moves (path escapes the
    // package root; cargo may warn and ignore — src/ still covers edits).
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let rev = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=FW_BUILD_TS={ts}");
    println!("cargo:rustc-env=FW_GIT_REV={rev}");
}
