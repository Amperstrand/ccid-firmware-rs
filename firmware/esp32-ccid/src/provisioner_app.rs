//! provisioner-min app mount point (M3 product preset, t_0492b7c6).
//!
//! Composition (products.toml `provisioner-min`): serving kernel
//! (crates/ccid-service) + NFC frontend + the **bolty-core provisioner
//! app** — card burn / wipe / derive-keys. That app lives in bolty-rs
//! and arrives here as a rev-pinned git dependency in a follow-up bolty
//! PR; its source is deliberately NOT vendored into this repo
//! (one-way-kernel doctrine, M2: the kernel homed here, bolty consumes
//! via git pin).
//!
//! Until it lands, the `provisioner` feature compiles this mount point
//! over the same kernel+frontend surface the app will use, so
//! `build.sh product=provisioner-min` and its CI artifact leg build the
//! real preset shape today — the app module slots in behind this gate
//! without touching the preset manifest or the dispatch.

/// Preset identity baked into every provisioner-min image (visible in
/// the map/ELF; the bolty app replaces this with its real entry point).
pub const PROVISIONER_PRESET: &str = "provisioner-min/1";
