//! Self-healing failure-streak tracker (issue #342).
//!
//! Pure decision logic, deliberately host-buildable: the F469
//! `SmartcardWrapper` (main.rs, arm-only) wires it around the USART
//! driver, so the policy — what counts as a failure, when the re-init
//! threshold fires, how the event surfaces — is unit-locked here
//! instead of living untestable inside the wrapper.
//!
//! The contract this module encodes:
//!
//! - **`Ok(0)` is failure-class.** A driver `Ok(0)` (zero bytes
//!   exchanged) is a mute card, not a success — it must never reset
//!   the streak. Before #342 the wrapper reset the streak on ANY
//!   `Ok(n)` including `n == 0`, so a mute card never reached
//!   [`REINIT_THRESHOLD`] and the self-healing power cycle never
//!   fired.
//! - **A fresh session starts a fresh streak** (Codex review #36: one
//!   early failure in a new session must not trigger a hidden cold
//!   reset on the next).
//! - **The re-init event latches** until the CCID layer consumes it
//!   ([`FailureTracker::take_reinit_pending`]): a re-init tore down
//!   and re-powered the card MID-SESSION, invalidating the session
//!   state both sides hold (ATR + negotiated parameters on ours, T=1
//!   link state on the host's) — the layer that owns the CCID session
//!   model must not stay silent about it.

/// Consecutive failure-class exchanges that trigger a driver-level
/// re-init (power cycle + re-activation of the card).
pub const REINIT_THRESHOLD: u8 = 3;

/// Classification of one driver transmit outcome for streak purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// `Ok(n)` with `n > 0` — bytes actually exchanged. Resets the
    /// streak.
    Success,
    /// `Ok(0)` — the card stayed mute for the whole exchange window.
    /// Failure-class (#342): accumulates the streak, never resets it.
    Mute,
    /// `Err(_)` — driver-level failure (timeout, protocol, hardware).
    /// Accumulates the streak.
    Failure,
}

impl Outcome {
    /// Classify a `Result<usize, E>` from a driver transmit.
    pub fn of_len(len: usize, is_err: bool) -> Self {
        if is_err {
            Outcome::Failure
        } else if len == 0 {
            Outcome::Mute
        } else {
            Outcome::Success
        }
    }
}

/// Streak + re-init accounting for one driver (issue #342).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureTracker {
    consecutive_failures: u8,
    reinit_count: u32,
    reinit_pending: bool,
}

impl FailureTracker {
    pub const fn new() -> Self {
        Self {
            consecutive_failures: 0,
            reinit_count: 0,
            reinit_pending: false,
        }
    }

    /// Session boundary (host IccPowerOn/IccPowerOff): a fresh session
    /// must not inherit the previous session's streak. The pending
    /// re-init latch deliberately survives — the CCID layer consumes it
    /// ([`Self::take_reinit_pending`]), no power transition may eat
    /// the event.
    pub fn session_start(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Record one transmit outcome. Returns `true` exactly when this
    /// outcome pushed the streak to [`REINIT_THRESHOLD`] — the caller
    /// then performs the power cycle; the event latches
    /// `reinit_pending` and the streak restarts.
    pub fn record(&mut self, outcome: Outcome) -> bool {
        match outcome {
            Outcome::Success => {
                self.consecutive_failures = 0;
                false
            }
            Outcome::Mute | Outcome::Failure => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                if self.consecutive_failures >= REINIT_THRESHOLD {
                    self.consecutive_failures = 0;
                    self.reinit_count = self.reinit_count.saturating_add(1);
                    self.reinit_pending = true;
                    return true;
                }
                false
            }
        }
    }

    /// Total self-healing re-inits since boot (Escape 0xD0 word).
    pub fn reinit_count(&self) -> u32 {
        self.reinit_count
    }

    /// Current streak (0 = last exchange succeeded).
    pub fn consecutive_failures(&self) -> u8 {
        self.consecutive_failures
    }

    /// Consume the pending re-init event — the CCID layer's inline
    /// indication that the card session was torn down mid-session.
    pub fn take_reinit_pending(&mut self) -> bool {
        core::mem::replace(&mut self.reinit_pending, false)
    }
}

impl Default for FailureTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_resets_the_streak() {
        let mut t = FailureTracker::new();
        assert!(!t.record(Outcome::Failure));
        assert!(!t.record(Outcome::Failure));
        assert!(!t.record(Outcome::Success));
        assert_eq!(t.consecutive_failures(), 0, "success clears the streak");
        // Two more failures after the reset stay below the threshold.
        assert!(!t.record(Outcome::Failure));
        assert!(!t.record(Outcome::Failure));
        assert_eq!(t.consecutive_failures(), 2);
    }

    #[test]
    fn mute_is_failure_class_and_never_resets_the_streak() {
        // THE #342 regression: a mute card yields Ok(0) forever — that
        // must accumulate toward the re-init, not reset it.
        let mut t = FailureTracker::new();
        assert!(!t.record(Outcome::Mute));
        assert_eq!(t.consecutive_failures(), 1);
        assert!(!t.record(Outcome::Mute));
        assert_eq!(t.consecutive_failures(), 2);
        assert!(t.record(Outcome::Mute), "third mute fires the re-init");
        assert_eq!(t.reinit_count(), 1);
    }

    #[test]
    fn threshold_fires_exactly_once_and_restarts_the_streak() {
        let mut t = FailureTracker::new();
        assert!(!t.record(Outcome::Failure));
        assert!(!t.record(Outcome::Failure));
        assert!(
            t.record(Outcome::Failure),
            "third consecutive failure fires"
        );
        assert!(!t.record(Outcome::Failure), "streak restarted at 0");
        assert!(!t.record(Outcome::Failure));
        assert!(t.record(Outcome::Failure), "and fires again on the next 3");
        assert_eq!(t.reinit_count(), 2);
    }

    #[test]
    fn mixed_mute_and_error_outcomes_accumulate_together() {
        // A mute exchange followed by hard errors is one streak of
        // failure-class outcomes — the card is equally dead either way.
        let mut t = FailureTracker::new();
        assert!(!t.record(Outcome::Mute));
        assert!(!t.record(Outcome::Failure));
        assert!(t.record(Outcome::Mute));
    }

    #[test]
    fn outcome_classification_from_result_shape() {
        assert_eq!(Outcome::of_len(2, false), Outcome::Success);
        assert_eq!(Outcome::of_len(255, false), Outcome::Success);
        assert_eq!(Outcome::of_len(0, false), Outcome::Mute);
        assert_eq!(Outcome::of_len(0, true), Outcome::Failure);
    }

    #[test]
    fn session_start_clears_the_streak_but_not_the_pending_event() {
        let mut t = FailureTracker::new();
        for _ in 0..3 {
            t.record(Outcome::Mute);
        }
        assert!(t.take_reinit_pending(), "first threshold run consumed");
        // a second threshold run latches again
        for _ in 0..3 {
            t.record(Outcome::Mute);
        }
        t.session_start();
        assert_eq!(t.consecutive_failures(), 0, "streak cleared");
        assert!(
            t.take_reinit_pending(),
            "the latch survives the session boundary — only take_ consumes it"
        );
        assert!(!t.take_reinit_pending(), "take_ consumes exactly once");
    }

    #[test]
    fn reinit_pending_latches_until_consumed() {
        let mut t = FailureTracker::new();
        assert!(!t.take_reinit_pending(), "nothing latched at boot");
        t.record(Outcome::Failure);
        t.record(Outcome::Failure);
        t.record(Outcome::Failure);
        assert!(t.take_reinit_pending(), "threshold run latched the event");
        assert!(!t.take_reinit_pending(), "consumed exactly once");
    }
}
