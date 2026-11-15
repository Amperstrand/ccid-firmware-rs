//! Phantom-ICC presence validation (#583).
//!
//! A contact frontend whose card-detect pin FLOATS when its slot shield
//! is absent (F469-DISCO + Specter DIY Shield removed, bench-verified
//! 2026-11-15) reports a card that never answers: pcscd exposes an
//! unresponsive slot and every `SCardConnect` burns the full activation
//! timeout (measured ~4 s/cycle on the bench).
//!
//! The validator latches ABSENT after N consecutive failed activations
//! while the pin stays asserted. A pin LOW observation always resets
//! the latch (a mechanical slot switch toggles on real insert/remove),
//! and a successful activation clears the strike count — so real-shield
//! semantics are unchanged: insert → pin toggles → fresh activation
//! attempts are believed again.
//!
//! Pure no_std state machine, host-testable (tests at the bottom of
//! this file mirror the bench sequence from issue #583).

#[derive(Debug, Clone, Copy)]
pub struct PresenceValidator {
    strikes: u8,
    threshold: u8,
    latched_absent: bool,
}

impl PresenceValidator {
    /// `threshold` = consecutive failed activations (with pin asserted)
    /// before the slot is reported absent. 0 disables validation
    /// (raw-pin passthrough — the pre-#583 behavior).
    pub const fn new(threshold: u8) -> Self {
        Self {
            strikes: 0,
            threshold,
            latched_absent: false,
        }
    }

    /// One observation.
    ///
    /// * `pin_asserted` — raw card-detect pin state (true = card edge).
    /// * `activation` — `Some(Ok(()))` / `Some(Err(()))` after an
    ///   activation (IccPowerOn) attempt concluded, `None` for a plain
    ///   presence poll with no new evidence.
    ///
    /// Returns the VALIDATED presence to answer GetSlotStatus with.
    pub fn observe(&mut self, pin_asserted: bool, activation: Option<Result<(), ()>>) -> bool {
        if self.threshold == 0 {
            return pin_asserted;
        }
        if !pin_asserted {
            // Deasserted pin: real removal (or shield reseat). Both the
            // strike count and the latch reset — the next asserted edge
            // is treated as a fresh insertion.
            self.strikes = 0;
            self.latched_absent = false;
            return false;
        }
        if self.latched_absent {
            // Pin still asserted but every activation failed: hold the
            // phantom absent until the pin toggles or a later manual
            // activation succeeds.
            return false;
        }
        match activation {
            None => true,
            Some(Ok(())) => {
                self.strikes = 0;
                true
            }
            Some(Err(())) => {
                self.strikes = self.strikes.saturating_add(1);
                if self.strikes >= self.threshold {
                    self.latched_absent = true;
                    return false;
                }
                true
            }
        }
    }

    /// True while the validator is holding a phantom-absent latch.
    pub fn is_latched_absent(&self) -> bool {
        self.latched_absent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_high_no_evidence_reports_present() {
        let mut v = PresenceValidator::new(3);
        assert!(v.observe(true, None));
    }

    #[test]
    fn bench_sequence_pin_floating_never_answers() {
        // Issue #583 sequence: pin asserted, activations fail.
        let mut v = PresenceValidator::new(3);
        assert!(v.observe(true, Some(Err(()))), "1st failure still present");
        assert!(v.observe(true, None), "poll between failures: present");
        assert!(v.observe(true, Some(Err(()))), "2nd failure still present");
        assert!(
            !v.observe(true, Some(Err(()))),
            "3rd failure latches ABSENT"
        );
        assert!(!v.observe(true, None), "polls after latch stay absent");
        assert!(
            !v.observe(true, Some(Err(()))),
            "later failures stay absent"
        );
    }

    #[test]
    fn pin_low_resets_the_latch() {
        let mut v = PresenceValidator::new(2);
        v.observe(true, Some(Err(())));
        v.observe(true, Some(Err(())));
        assert!(v.is_latched_absent());
        // shield/card motion toggles the pin — fresh insertion believed
        assert!(!v.observe(false, None), "pin low = absent");
        assert!(!v.is_latched_absent(), "latch cleared");
        assert!(v.observe(true, None), "fresh insert reports present");
    }

    #[test]
    fn successful_activation_clears_strikes() {
        let mut v = PresenceValidator::new(2);
        v.observe(true, Some(Err(())));
        assert!(v.observe(true, Some(Ok(()))), "success keeps present");
        // strikes were reset by the success: a SINGLE later failure must
        // not latch at threshold 2
        assert!(
            v.observe(true, Some(Err(()))),
            "one failure after a success does not latch (strikes reset)"
        );
    }

    #[test]
    fn threshold_zero_is_passthrough() {
        let mut v = PresenceValidator::new(0);
        for _ in 0..10 {
            v.observe(true, Some(Err(())));
        }
        assert!(v.observe(true, None), "disabled validator = raw pin");
    }
}
