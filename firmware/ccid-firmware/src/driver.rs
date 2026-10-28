use ccid_core::Diagnostics;

pub trait SmartcardDriver {
    type Error: core::fmt::Debug;

    fn power_on(&mut self) -> core::result::Result<&[u8], Self::Error>;
    fn power_off(&mut self);
    fn is_card_present(&self) -> bool;
    fn transmit_apdu(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error>;
    fn transmit_raw(
        &mut self,
        data: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error>;
    fn set_protocol(&mut self, protocol: u8);
    fn get_protocol(&self) -> u8 {
        0
    }
    fn set_clock(&mut self, _enable: bool) {}
    fn set_clock_and_rate(
        &mut self,
        _clock_hz: u32,
        _rate_bps: u32,
    ) -> core::result::Result<(u32, u32), Self::Error>;
    /// Driver-level self-healing re-init count, surfaced through the
    /// core's 0xD0 diagnostics snapshot (card-interface
    /// `CardBackend::reinit_count` semantics). Drivers without
    /// self-healing keep the 0.
    fn reinit_count(&self) -> u32 {
        0
    }
    /// Consume the pending self-healing re-init event (issue #342):
    /// true exactly once after a re-init tore down and re-powered the
    /// card mid-session. The CCID layer polls this after command
    /// dispatch — the card session (ATR, negotiated parameters, T=1
    /// link state) was invalidated and the session-owning layer must
    /// not stay silent about it. Drivers without self-healing never
    /// latch it.
    fn take_reinit_pending(&mut self) -> bool {
        false
    }
    /// Fold driver-known state into the Escape 0xD0 diagnostics
    /// snapshot (issue #344).
    ///
    /// `base` arrives carrying the handler's LIVE counters
    /// (apdu_tx/rx, nak, error, uptime) — core-owned since the #268
    /// fix retired the old driver-side stub, which fabricated zeros
    /// for them on the F469. The default leaves it untouched; drivers
    /// whose hardware knows more than the polled presence cache
    /// override this — the F746 bit-bang wrapper stamps its live
    /// presence-pin reading, so a 0xD0 query between presence polls
    /// can never answer the fabricated all-zero default with a card
    /// inserted. The answer is serialized in the frozen ccid-core
    /// 28-byte layout (see `ccid_core::Diagnostics`).
    fn diagnostics(&self, base: &mut Diagnostics) {
        let _ = base;
    }
}
