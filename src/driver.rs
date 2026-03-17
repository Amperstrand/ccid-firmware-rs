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
    fn set_clock(&mut self, _enable: bool) {}
    fn set_clock_and_rate(
        &mut self,
        _clock_hz: u32,
        _rate_bps: u32,
    ) -> core::result::Result<(u32, u32), Self::Error>;
}
