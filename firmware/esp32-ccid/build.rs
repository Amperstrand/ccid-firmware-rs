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
    // Watch the RESOLVED git ref, not just .git/HEAD (Codex #85, issue
    // #112): on a branch HEAD is a symref whose bytes never change across
    // a commit advance — the loose ref file it points at is what gets
    // rewritten. Watching HEAD alone left FW_GIT_REV one commit stale
    // until something else dirtied the fingerprint. `rev-parse --git-dir`
    // is worktree-aware (linked worktrees keep per-worktree refs under
    // .git/worktrees/<name>/), so the resolved paths are correct there
    // too. Cargo tracks absolute rerun-if-changed paths outside the
    // package root (verified on the bench: a commit marks the package
    // Dirty on the ref file and reruns this script).
    if let Some(git_dir) = git_dir() {
        let head_path = git_dir.join("HEAD");
        watch(&head_path);
        if let Ok(head) = std::fs::read_to_string(&head_path) {
            if let Some(refname) = head.strip_prefix("ref: ").map(str::trim) {
                // Loose ref — rewritten by every commit on this branch.
                // May be absent (packed refs); packed-refs below covers it.
                watch(&git_dir.join(refname));
            }
        }
        // After `git gc`/clone the ref lives in packed-refs instead of the
        // loose file — updates land there.
        watch(&git_dir.join("packed-refs"));
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let rev = git_head_short();
    println!("cargo:rustc-env=FW_BUILD_TS={ts}");
    println!("cargo:rustc-env=FW_GIT_REV={rev}");
}

/// The git dir owning this checkout/worktree (`git rev-parse --git-dir`).
/// From the package dir git prints a repo-relative path for top-level
/// checkouts — anchor it to the build script's cwd.
fn git_dir() -> Option<std::path::PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return None;
    }
    let p = std::path::PathBuf::from(s);
    Some(if p.is_absolute() {
        p
    } else {
        std::env::current_dir().ok()?.join(p)
    })
}

fn git_head_short() -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Emit rerun-if-changed only for files that exist — cargo warns on
/// missing paths, and the loose ref may legitimately be packed away.
fn watch(path: &std::path::Path) {
    if path.is_file() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
