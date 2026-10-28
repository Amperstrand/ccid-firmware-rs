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

    /// Apply host-negotiated wire timing after an accepted
    /// SetParameters/ResetParameters (#122): Fi/Di + guard time N +
    /// protocol, decoded by the core (`accepted_contact_timing`) from
    /// the CCID abProtocolDataStructure. This is the dwFeatures bit-5
    /// contract ("Automatic baud rate change according to frequency and
    /// Fi, Di params" — all three of our profiles advertise it).
    ///
    /// Implementations must stay SAFE when the requested rate
    /// contradicts what the CARD actually negotiated: the Cherry
    /// profile additionally advertises bit 7 (reader-owned PPS), so
    /// libccid skips its own PPS and sends Fi/Di derived from the ATR
    /// TA1 REGARDLESS — with the reader's own PPS speed-up gated off
    /// the card still runs at the ATR-default Di and the wire must NOT
    /// follow the host's belief (the f5feb61 bench-regression class).
    /// The default is a no-op; the USART frontend applies guard time
    /// always (TX pacing only) and the baud only when it matches the
    /// card's proven state.
    fn set_wire_timing(&mut self, _fi: u16, _di: u8, _guard_time_n: u8) {}

    /// Classify a failed exchange for the CCID error-map table (#136,
    /// forwarded by `SmartcardCoreAdapter` into
    /// `CardBackend::xfr_error_class`). Concrete frontends with a
    /// known error enum override this so parity surfaces as CCID
    /// bError 0xFD; the default keeps the historical fold.
    fn xfr_error_class(&self, _err: &Self::Error) -> card_interface::XfrErrorKind {
        card_interface::XfrErrorKind::Unclassified
    }

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
