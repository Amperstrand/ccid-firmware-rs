//! ESP-IDF arm of the serve-proven OTA validity gate (#294 WS2).
//!
//! Every shipped `.full` config now sets
//! `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` (WS2 fix a) — a freshly
//! booted OTA image runs pending-verify and reverts on the next reset
//! unless the app proves itself. The proof is SERVE EVIDENCE: the
//! serving kernel (`ccid_service::ota_validity`) trips the gate on the
//! FIRST completed CCID exchange (first `ServeAction::Respond` through
//! the GemPC serial kernel — CDC/UART/SPP/TCP; first sent vpcd answer).
//!
//! Every OTA-capable serving main calls [`arm()`] before entering its
//! serve loop:
//!
//! * `pn7160_ccid::run` (c3-ccid / c3-ccid-ble / c3-ccid-ble-diag)
//! * `wifi_ccid::run` (wifi-ccid)
//! * `wifi_vpcd::run` (wifi-vpcd nucula + m5stick-vpcd)
//! * the mfrc522 serial main (m5stick / m5stick-plus-lcd / m5atom and
//!   their ble/spp variants — UART and SPP both feed the kernel)
//! * the pn532 serial main (feature-only composition, same serve path)
//!
//! Deliberately NOT armed — diagnostic mains that never serve CCID
//! (pn7160_bringup, pn7160_bitbang, pn7160_m1, pn7160_v10raw,
//! pn7160_actdiag, pad_diag): for a diagnostic image, rolling back to
//! the previous serving image is the safe direction, and
//! `wifi_common::station_bring_up`'s old eager mark (fired right after
//! the wifi-connect attempt) is gone — the mark now means "this image
//! just served a host".

/// Arm the serve-proven OTA validity gate: install the ESP-IDF
/// mark-valid call into the serving kernel. Call once, before serving;
/// the kernel fires it on the first completed CCID exchange and never
/// again.
pub fn arm() {
    ccid_service::ota_validity::install_mark_valid(mark_valid);
}

/// The platform mark-valid call. Returns the ESP-IDF rc: 0 = marked
/// (was pending-verify), nonzero = not in the pending state (first
/// boot, factory slot, or a bootloader without the rollback state
/// machine) — logged by the gate, never fatal.
fn mark_valid() -> i32 {
    unsafe { esp_idf_sys::esp_ota_mark_app_valid_cancel_rollback() }
}
