//! Serve-proven OTA rollback validity gate (#294 WS2).
//!
//! ESP-IDF app-rollback semantics (research in
//! `tests/hardware/ota/README.md`, vendored IDF v5.5.5): with
//! `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` baked into the bootloader, a
//! freshly booted OTA image runs in the `ESP_OTA_IMG_PENDING_VERIFY`
//! state and reverts to the previous slot on the next reset unless the
//! app calls `esp_ota_mark_app_valid_cancel_rollback()`. The image gets
//! exactly one boot to prove itself.
//!
//! The proof this platform demands is **serve evidence**: the first
//! complete CCID request→response exchange through the serving kernel —
//! the same bar the boot-smoke gate uses (a GetSlotStatus round-trip
//! exercises the full parse → handler → respond stack, no card needed).
//! Before WS2 the wiring was inverted: the only mark-valid caller fired
//! eagerly right after the wifi-connect attempt (no serve proof,
//! reachable from one profile family only) while every profile whose
//! bootloader had the rollback state machine compiled in never marked
//! at all — an OTA-updated image reverted on the next boot. This gate
//! is the fixed shape: the kernel itself trips it on the first
//! [`ServeAction::Respond`](crate::ccid_serial_server::ServeAction::Respond)
//! (GemPC serial kernel — CDC/UART/SPP/TCP all feed it) and on the
//! first successfully-sent vpcd answer.
//!
//! Boundary (lib.rs): the kernel cannot call ESP-IDF, so the platform
//! mark-valid fn is INSTALLED by the firmware bin — the same
//! installed-sink pattern as the handler's card-presence sink. Unset =
//! inert: host tests, non-OTA builds, and the diagnostic mains
//! (bringup/bitbang/m1/v10raw/actdiag) serve no OTA self-test by
//! design (a diagnostic image that rolls back is the safe direction).
//!
//! A pending-verify image that never completes one serve exchange
//! still reverts — that is the defense working, not a regression.

use std::sync::Mutex;

/// The one-boot state machine, struct-ified (the `#350` `AbsenceGate`
/// pattern) so the semantics are unit-testable without process-global
/// state. The live gate is [`GATE`] below.
#[derive(Debug, Default)]
pub struct ValidityGate {
    /// The platform mark-valid call, installed by the firmware bin.
    marker: Option<fn() -> i32>,
    /// Set once the first serve evidence ran the marker — every later
    /// exchange is a plain load; the marker runs at most once per boot
    /// by construction.
    marked: bool,
}

impl ValidityGate {
    pub const fn new() -> Self {
        Self {
            marker: None,
            marked: false,
        }
    }

    /// Whether a marker is installed (diagnostic/test surface).
    pub fn armed(&self) -> bool {
        self.marker.is_some()
    }

    /// Install the platform mark-valid call. Returns `false` when the
    /// gate already ran a marker this boot (late arming after the
    /// first evidence is a no-op — evidence observed while disarmed
    /// does not latch).
    pub fn arm(&mut self, marker: fn() -> i32) -> bool {
        if self.marked {
            return false;
        }
        self.marker = Some(marker);
        true
    }

    /// Serve evidence, fired by the serving kernel on every completed
    /// host exchange. The FIRST evidence (while armed) runs the
    /// installed marker exactly once and logs the platform rc
    /// (0 = marked valid; nonzero = not pending-verify — first boot,
    /// factory slot, or rollback disabled — logged, never fatal).
    /// Every later signal is a no-op. Returns `true` when THIS call
    /// ran the marker. `source` names the transport for the log line
    /// (e.g. `"gempc-serial"`, `"vpcd"`).
    pub fn note_serve_evidence(&mut self, source: &str) -> bool {
        let Some(marker) = self.marker else {
            // Unarmed: host tests, non-OTA builds, diagnostic mains.
            // Debug, not warn: every unarmed exchange passes here.
            log::debug!("ota-validity: serve evidence ({source}) — gate not armed");
            return false;
        };
        if self.marked {
            return false;
        }
        self.marked = true;
        let rc = marker();
        if rc == 0 {
            log::info!("ota-validity: first serve evidence ({source}) — image marked valid");
        } else {
            log::warn!(
                "ota-validity: first serve evidence ({source}) — mark rc={rc} \
                 (not pending-verify: first boot, factory slot, or rollback disabled)"
            );
        }
        true
    }
}

/// The live, process-global gate. Mutex only because a static must be
/// `Sync` (the `#350` precedent); contention is nil — one CAS-protected
/// load per served exchange after the first.
static GATE: Mutex<ValidityGate> = Mutex::new(ValidityGate::new());

/// Arm the gate from the OTA-capable main BEFORE serving: install the
/// platform mark-valid call. `marker` must be thread-safe (it runs on
/// whichever task first completes a serve exchange).
pub fn install_mark_valid(marker: fn() -> i32) {
    GATE.lock().unwrap_or_else(|p| p.into_inner()).arm(marker);
}

/// Whether the gate is armed (diagnostic surface).
pub fn armed() -> bool {
    GATE.lock().unwrap_or_else(|p| p.into_inner()).armed()
}

/// Serve evidence — the kernel-side entry point. See
/// [`ValidityGate::note_serve_evidence`].
pub fn note_serve_evidence(source: &str) -> bool {
    GATE.lock()
        .unwrap_or_else(|p| p.into_inner())
        .note_serve_evidence(source)
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Reset the process-global gate between the (single) global-layer
    //! test and the kernel-wiring test — `ValidityGate` itself is tested
    //! on private instances below, race-free by construction.

    use super::*;

    /// Test-only: disarm + unlatch the global gate. Only tests that
    /// serialize on [`GATE_TEST_LOCK`] may call it.
    pub static GATE_TEST_LOCK: Mutex<()> = Mutex::new(());

    pub fn reset_global_gate() {
        let mut gate = GATE.lock().unwrap_or_else(|p| p.into_inner());
        *gate = ValidityGate::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicUsize, Ordering};

    // Codex P2 on the head review (test isolation): the fn-pointer
    // shape (no closures, no context) is the price of the
    // installed-sink pattern shared with the card-presence sink — so
    // every test gets its OWN marker + OWN counter (parallel-runner
    // safe by construction; nothing is shared between tests). Only the
    // two global-gate tests share state, and they serialize on
    // GATE_TEST_LOCK with race-tolerant assertions (any sibling test's
    // unarmed traffic is inert; armed traffic can only be the single
    // latch-consuming firing).

    static RUNS_A: AtomicUsize = AtomicUsize::new(0);
    fn marker_a() -> i32 {
        RUNS_A.fetch_add(1, Ordering::SeqCst);
        0
    }
    static RUNS_B: AtomicUsize = AtomicUsize::new(0);
    fn marker_b() -> i32 {
        RUNS_B.fetch_add(1, Ordering::SeqCst);
        0
    }
    static RUNS_C: AtomicUsize = AtomicUsize::new(0);
    fn marker_c() -> i32 {
        // nonzero rc: the "not pending-verify" arm (warn, not fatal).
        RUNS_C.fetch_add(1, Ordering::SeqCst);
        1
    }
    static RUNS_D: AtomicUsize = AtomicUsize::new(0);
    fn marker_d() -> i32 {
        RUNS_D.fetch_add(1, Ordering::SeqCst);
        0
    }
    static RUNS_G: AtomicUsize = AtomicUsize::new(0);
    fn marker_g() -> i32 {
        RUNS_G.fetch_add(1, Ordering::SeqCst);
        0
    }

    #[test]
    fn unarmed_gate_is_inert_and_does_not_latch() {
        let mut gate = ValidityGate::new();
        assert!(!gate.armed());
        // Evidence while disarmed: noted, nothing to mark...
        assert!(!gate.note_serve_evidence("test"));
        assert!(!gate.note_serve_evidence("test"));
        assert_eq!(RUNS_A.load(Ordering::SeqCst), 0, "nothing ran");
        // ...and it does NOT consume the gate: arming afterwards still
        // lets the NEXT evidence run the marker.
        assert!(gate.arm(marker_a));
        assert!(gate.note_serve_evidence("test"));
        assert_eq!(RUNS_A.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn first_evidence_runs_marker_once_then_idempotent() {
        let mut gate = ValidityGate::new();
        assert!(gate.arm(marker_b));
        assert!(gate.note_serve_evidence("gempc-uart"));
        // Every later exchange is a no-op — fire-once per boot.
        assert!(!gate.note_serve_evidence("gempc-uart"));
        assert!(!gate.note_serve_evidence("vpcd"));
        assert_eq!(RUNS_B.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn nonzero_rc_is_logged_not_fatal() {
        // The "not pending-verify" shape (first boot / factory slot /
        // rollback-disabled bootloader): rc != 0 must not panic and the
        // evidence still counts as consumed.
        let mut gate = ValidityGate::new();
        assert!(gate.arm(marker_c));
        assert!(gate.note_serve_evidence("vpcd"));
        assert_eq!(RUNS_C.load(Ordering::SeqCst), 1);
        assert!(!gate.note_serve_evidence("vpcd"));
        assert_eq!(RUNS_C.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn late_arm_after_first_evidence_is_rejected() {
        // Arming after the marker already ran cannot re-arm the boot.
        let mut gate = ValidityGate::new();
        assert!(gate.arm(marker_d));
        assert!(gate.note_serve_evidence("gempc-cdc"));
        assert!(!gate.arm(marker_d), "no second chance per boot");
        assert_eq!(RUNS_D.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn global_layer_round_trip() {
        // The installed-fn layer (what the firmware bin and the kernel
        // write sites call). Race-tolerant by construction (Codex P2):
        // do NOT assert ownership of the first signal — a sibling
        // kernel test's delivered response may be the exchange that
        // consumes the latch. Assert instead that exactly ONE firing
        // happened (latch-bounded either way) and that further
        // evidence never re-marks.
        let _lock = test_support::GATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        test_support::reset_global_gate();
        assert!(!armed());
        install_mark_valid(marker_g);
        assert!(armed());

        let _ = note_serve_evidence("global-test");
        let after_first = RUNS_G.load(Ordering::SeqCst);
        assert_eq!(
            after_first, 1,
            "latch guarantees exactly one firing once any evidence arrived"
        );
        let _ = note_serve_evidence("global-test");
        assert_eq!(
            RUNS_G.load(Ordering::SeqCst),
            after_first,
            "later evidence never re-marks"
        );

        test_support::reset_global_gate();
        assert!(!armed());
    }
}
