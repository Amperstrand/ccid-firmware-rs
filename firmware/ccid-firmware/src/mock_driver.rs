use crate::driver::SmartcardDriver;

#[derive(Debug, Clone)]
pub struct MockError(pub &'static str);

impl core::fmt::Display for MockError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

const MAX_ATR_LEN: usize = 33;
const MAX_APDU_RESPONSES: usize = 16;
const MAX_APDU_LEN: usize = 261;
const MAX_CALL_LOG: usize = 64;

pub struct MockSmartcardDriver {
    card_present: bool,
    atr: [u8; MAX_ATR_LEN],
    atr_len: usize,
    protocol: u8,
    apdu_responses: [[u8; MAX_APDU_LEN]; MAX_APDU_RESPONSES],
    apdu_response_lens: [usize; MAX_APDU_RESPONSES],
    apdu_response_count: usize,
    apdu_response_idx: usize,
    raw_responses: [[u8; MAX_APDU_LEN]; MAX_APDU_RESPONSES],
    raw_response_lens: [usize; MAX_APDU_RESPONSES],
    raw_response_count: usize,
    raw_response_idx: usize,
    power_on_should_fail: bool,
    transmit_apdu_should_fail: bool,
    call_log: [MockCall; MAX_CALL_LOG],
    call_log_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MockCall {
    PowerOn,
    PowerOff,
    IsCardPresent,
    TransmitApdu { cmd_len: usize },
    TransmitRaw { data_len: usize },
    SetProtocol { protocol: u8 },
    SetClock { enable: bool },
    SetClockAndRate { clock_hz: u32, rate_bps: u32 },
}

impl MockSmartcardDriver {
    pub fn new() -> Self {
        Self {
            card_present: false,
            atr: [0u8; MAX_ATR_LEN],
            atr_len: 0,
            protocol: 0,
            apdu_responses: [[0u8; MAX_APDU_LEN]; MAX_APDU_RESPONSES],
            apdu_response_lens: [0usize; MAX_APDU_RESPONSES],
            apdu_response_count: 0,
            apdu_response_idx: 0,
            raw_responses: [[0u8; MAX_APDU_LEN]; MAX_APDU_RESPONSES],
            raw_response_lens: [0usize; MAX_APDU_RESPONSES],
            raw_response_count: 0,
            raw_response_idx: 0,
            power_on_should_fail: false,
            transmit_apdu_should_fail: false,
            call_log: [MockCall::PowerOff; MAX_CALL_LOG],
            call_log_len: 0,
        }
    }

    pub fn card_present(mut self, present: bool) -> Self {
        self.card_present = present;
        self
    }

    pub fn with_atr(mut self, atr: &[u8]) -> Self {
        let len = atr.len().min(MAX_ATR_LEN);
        self.atr[..len].copy_from_slice(&atr[..len]);
        self.atr_len = len;
        self
    }

    pub fn with_apdu_response(mut self, response: &[u8]) -> Self {
        if self.apdu_response_count < MAX_APDU_RESPONSES {
            let idx = self.apdu_response_count;
            let len = response.len().min(MAX_APDU_LEN);
            self.apdu_responses[idx][..len].copy_from_slice(&response[..len]);
            self.apdu_response_lens[idx] = len;
            self.apdu_response_count += 1;
        }
        self
    }

    pub fn with_raw_response(mut self, response: &[u8]) -> Self {
        if self.raw_response_count < MAX_APDU_RESPONSES {
            let idx = self.raw_response_count;
            let len = response.len().min(MAX_APDU_LEN);
            self.raw_responses[idx][..len].copy_from_slice(&response[..len]);
            self.raw_response_lens[idx] = len;
            self.raw_response_count += 1;
        }
        self
    }

    pub fn with_protocol(mut self, protocol: u8) -> Self {
        self.protocol = protocol;
        self
    }

    pub fn with_power_on_error(mut self) -> Self {
        self.power_on_should_fail = true;
        self
    }

    pub fn with_transmit_apdu_error(mut self) -> Self {
        self.transmit_apdu_should_fail = true;
        self
    }

    pub fn call_log(&self) -> &[MockCall] {
        &self.call_log[..self.call_log_len]
    }

    pub fn reset_call_log(&mut self) {
        self.call_log_len = 0;
    }

    fn log(&mut self, call: MockCall) {
        if self.call_log_len < MAX_CALL_LOG {
            self.call_log[self.call_log_len] = call;
            self.call_log_len += 1;
        }
    }
}

impl Default for MockSmartcardDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl SmartcardDriver for MockSmartcardDriver {
    type Error = MockError;

    fn power_on(&mut self) -> core::result::Result<&[u8], Self::Error> {
        self.log(MockCall::PowerOn);
        if self.power_on_should_fail {
            Err(MockError("power on failed"))
        } else {
            Ok(&self.atr[..self.atr_len])
        }
    }

    fn power_off(&mut self) {
        self.log(MockCall::PowerOff);
        self.protocol = 0;
    }

    fn is_card_present(&self) -> bool {
        self.card_present
    }

    fn transmit_apdu(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error> {
        self.log(MockCall::TransmitApdu {
            cmd_len: command.len(),
        });
        if self.transmit_apdu_should_fail {
            return Err(MockError("transmit failed"));
        }
        if self.apdu_response_idx < self.apdu_response_count {
            let idx = self.apdu_response_idx;
            self.apdu_response_idx += 1;
            let len = self.apdu_response_lens[idx];
            let copy_len = len.min(response.len());
            response[..copy_len].copy_from_slice(&self.apdu_responses[idx][..copy_len]);
            Ok(copy_len)
        } else {
            Ok(0)
        }
    }

    fn transmit_raw(
        &mut self,
        data: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error> {
        self.log(MockCall::TransmitRaw {
            data_len: data.len(),
        });
        if self.raw_response_idx < self.raw_response_count {
            let idx = self.raw_response_idx;
            self.raw_response_idx += 1;
            let len = self.raw_response_lens[idx];
            let copy_len = len.min(response.len());
            response[..copy_len].copy_from_slice(&self.raw_responses[idx][..copy_len]);
            Ok(copy_len)
        } else {
            Ok(0)
        }
    }

    fn set_protocol(&mut self, protocol: u8) {
        self.log(MockCall::SetProtocol { protocol });
        self.protocol = protocol;
    }

    fn set_clock(&mut self, enable: bool) {
        self.log(MockCall::SetClock { enable });
    }

    fn set_clock_and_rate(
        &mut self,
        clock_hz: u32,
        rate_bps: u32,
    ) -> core::result::Result<(u32, u32), Self::Error> {
        self.log(MockCall::SetClockAndRate { clock_hz, rate_bps });
        Ok((clock_hz, rate_bps))
    }
}
