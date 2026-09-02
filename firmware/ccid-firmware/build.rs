use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

fn get_git_version() -> String {
    // GIT_VERSION=<x> override pins the version (reproducible builds); the
    // rerun-if-env-changed + rustc-env directives below keep it from going stale (#55).
    if let Ok(pinned) = env::var("GIT_VERSION") {
        if !pinned.is_empty() {
            return pinned;
        }
    }

    if let Ok(output) = Command::new("git")
        .args(["describe", "--tags", "--exact-match"])
        .output()
    {
        let tag = String::from_utf8_lossy(&output.stdout);
        let tag = tag.trim();
        if !tag.is_empty() {
            return tag.to_string();
        }
    }

    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
    {
        let hash = String::from_utf8_lossy(&output.stdout);
        let hash = hash.trim();
        if !hash.is_empty() {
            return format!("git:{}", hash);
        }
    }

    env!("CARGO_PKG_VERSION").to_string()
}

fn main() {
    let out_dir = env::var_os("OUT_DIR").unwrap();

    let memory_src = if env::var("CARGO_FEATURE_STM32F746").is_ok() {
        Some(Path::new("memory-f746.x"))
    } else if env::var("CARGO_FEATURE_STM32F469").is_ok() {
        Some(Path::new("memory.x"))
    } else {
        None
    };

    if let Some(src) = memory_src {
        if src.exists() {
            let out_memory_x = Path::new(&out_dir).join("memory.x");
            fs::copy(src, out_memory_x).unwrap();
        }
    }

    let version = get_git_version();
    println!("cargo:rustc-env=GIT_VERSION={}", version);
    println!("cargo:rerun-if-env-changed=GIT_VERSION");
    println!("cargo:rustc-link-search={}", out_dir.to_str().unwrap());
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=memory-f746.x");
    println!("cargo:rerun-if-changed=build.rs");
}
