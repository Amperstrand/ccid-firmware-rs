//! PN7160 NFC driver: NfcDriver-shaped session management over any
//! `Transport`. All protocol logic lives in the reader/bring-up modules;
//! this layer manages session state and buffer handling. Host-testable
//! via the mock Transport; the firmware crate wraps it in a thin
//! `NfcDriver` delegation.

use super::{reader, Transport};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    BringUp(&'static str),
    Select(&'static str),
    NoCard,
    ExchangeFailed,
    BufferTooSmall,
}

pub struct Pn7160Driver<T: Transport> {
    transport: T,
    active: bool,
}

impl<T: Transport> Pn7160Driver<T> {
    pub fn new(transport: T) -> Self {
        Pn7160Driver { transport, active: false }
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn into_transport(self) -> T {
        self.transport
    }
}

impl<T: Transport> Pn7160Driver<T> {
    /// NCI bring-up ladder (CORE_RESET → CORE_INIT → SET_CONFIG →
    /// DISCOVER_MAP → DISCOVER). Call once after VEN power-cycle.
    pub fn init(&mut self) -> Result<(), Error> {
        super::run_ladder(&mut self.transport).map_err(Error::BringUp)
    }

    /// Check whether a tag is in the field (consumes pending notifications).
    pub fn is_card_present(&mut self) -> bool {
        reader::wait_for_discovery(&mut self.transport).is_some()
    }

    /// Discover, select, and activate a tag; copies the ATS (from the
    /// activation notification's Initial_Params — NCI §6.3.4) into `atr`.
    pub fn power_on(&mut self, atr: &mut [u8]) -> Result<usize, Error> {
        let ntf =
            reader::wait_for_discovery(&mut self.transport).ok_or(Error::NoCard)?;
        let activation = reader::select_tag(&mut self.transport, &ntf)
            .map_err(Error::Select)?;
        let ats = reader::extract_ats(&activation)
            .ok_or(Error::Select("activation carries no ATS"))?;
        if atr.len() < ats.len() {
            return Err(Error::BufferTooSmall);
        }
        atr[..ats.len()].copy_from_slice(ats);
        self.active = true;
        Ok(ats.len())
    }

    /// Deactivate the RF interface back to idle.
    pub fn power_off(&mut self) {
        let _ = reader::deactivate_idle(&mut self.transport);
        self.active = false;
    }

    /// Exchange one APDU with the activated tag (connection 0).
    pub fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Error> {
        if !self.active {
            return Err(Error::NoCard);
        }
        let rsp = reader::exchange(&mut self.transport, 0, command)
            .ok_or(Error::ExchangeFailed)?;
        if response.len() < rsp.len() {
            return Err(Error::BufferTooSmall);
        }
        response[..rsp.len()].copy_from_slice(&rsp);
        Ok(rsp.len())
    }

    pub fn session_active(&self) -> bool {
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockTransport;
    use crate::{
        DEACTIVATE_TYPE_IDLE, GID_CORE, GID_RF, MT_NTF, MT_RSP,
        NCI_INTERFACE_ISO_DEP, NCI_PROTOCOL_ISO_DEP, NTF_RF_DEACTIVATE,
        NTF_RF_DISCOVER, NTF_RF_INTF_ACTIVATED, OID_CORE_INIT,
        OID_CORE_RESET, OID_CORE_SET_CONFIG, OID_RF_DEACTIVATE,
        OID_RF_DISCOVER, OID_RF_DISCOVER_MAP, OID_RF_DISCOVER_SELECT, STATUS_OK,
    };

    // A realistic ISO-DEP ATS (per ISO 14443-4: TL, T0, TA1, TB1)
    const TEST_ATS: [u8; 4] = [0x75, 0x77, 0x81, 0x02];

    fn script_ladder(t: &mut MockTransport) {
        t.push_reply(&[MT_RSP | GID_CORE, OID_CORE_RESET, 0x01, STATUS_OK]);
        t.push_notification(&[MT_NTF, OID_CORE_RESET, 0x01, 0x00]);
        t.push_reply(&[MT_RSP | GID_CORE, OID_CORE_INIT, 0x01, STATUS_OK]);
        t.push_reply(&[MT_RSP | GID_CORE, OID_CORE_SET_CONFIG, 0x01, STATUS_OK]);
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER_MAP, 0x01, STATUS_OK]);
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER, 0x01, STATUS_OK]);
    }

    fn script_discover_select_activate(t: &mut MockTransport) {
        // DISCOVER_NTF: [disc_id, proto, tech, params_len=0, interface]
        t.push_notification(&[MT_NTF | GID_RF, NTF_RF_DISCOVER, 0x05,
            0x01, NCI_PROTOCOL_ISO_DEP, 0x00, 0x00, NCI_INTERFACE_ISO_DEP]);
        // SELECT RSP
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER_SELECT, 0x01, STATUS_OK]);
        // INTF_ACTIVATED NTF with ATS in Initial_Params (NCI §6.3.4):
        // [id, intf, proto, tech, max_payload, params_len, ...ATS]
        t.push_notification(&[MT_NTF | GID_RF, NTF_RF_INTF_ACTIVATED, 0x0A,
            0x01, NCI_INTERFACE_ISO_DEP, NCI_PROTOCOL_ISO_DEP, 0x00, 0xFF,
            TEST_ATS.len() as u8, TEST_ATS[0], TEST_ATS[1], TEST_ATS[2], TEST_ATS[3]]);
    }

    fn script_apdu(t: &mut MockTransport, response: &[u8]) {
        let mut data = heapless::Vec::<u8, 258>::new();
        let _ = data.extend_from_slice(&[0x00, 0x00, response.len() as u8]);
        let _ = data.extend_from_slice(response);
        t.push_reply(&data);
    }

    fn script_deactivate(t: &mut MockTransport) {
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DEACTIVATE, 0x01, STATUS_OK]);
        t.push_notification(&[MT_NTF | GID_RF, NTF_RF_DEACTIVATE, 0x01,
            DEACTIVATE_TYPE_IDLE]);
    }

    #[test]
    fn full_session() {
        let mut t = MockTransport::new();
        script_ladder(&mut t);
        script_discover_select_activate(&mut t);
        script_apdu(&mut t, &[0x90, 0x00]);
        script_deactivate(&mut t);

        let mut drv = Pn7160Driver::new(t);

        drv.init().expect("ladder");

        let mut atr = [0u8; 32];
        let atr_len = drv.power_on(&mut atr).expect("power_on");
        assert_eq!(&atr[..atr_len], &TEST_ATS);
        assert!(drv.session_active());

        let mut rsp = [0u8; 256];
        let rsp_len = drv.transmit_apdu(&[0x00, 0xA4, 0x04, 0x00], &mut rsp).expect("apdu");
        assert_eq!(&rsp[..rsp_len], &[0x90, 0x00]);

        drv.power_off();
        assert!(!drv.session_active());
    }

    #[test]
    fn init_fails_on_empty_transport() {
        let mut drv = Pn7160Driver::new(MockTransport::new());
        assert!(drv.init().is_err());
    }

    #[test]
    fn power_on_no_card() {
        let mut t = MockTransport::new();
        script_ladder(&mut t);
        // no discovery NTF scripted

        let mut drv = Pn7160Driver::new(t);
        drv.init().expect("ladder");

        let mut atr = [0u8; 32];
        assert_eq!(drv.power_on(&mut atr), Err(Error::NoCard));
    }

    #[test]
    fn transmit_without_power_on() {
        let mut drv = Pn7160Driver::new(MockTransport::new());
        let mut rsp = [0u8; 256];
        assert_eq!(drv.transmit_apdu(&[0x00], &mut rsp), Err(Error::NoCard));
    }

    #[test]
    fn atr_buffer_too_small() {
        let mut t = MockTransport::new();
        script_discover_select_activate(&mut t);

        let mut drv = Pn7160Driver::new(t);
        let mut tiny_atr = [0u8; 2]; // TEST_ATS is 4 bytes
        assert_eq!(drv.power_on(&mut tiny_atr), Err(Error::BufferTooSmall));
    }

    #[test]
    fn apdu_response_buffer_too_small() {
        let mut t = MockTransport::new();
        script_discover_select_activate(&mut t);
        script_apdu(&mut t, &[0x90, 0x00, 0xAA, 0xBB]);

        let mut drv = Pn7160Driver::new(t);
        let mut atr = [0u8; 32];
        drv.power_on(&mut atr).expect("power_on");

        let mut tiny_rsp = [0u8; 2]; // response is 4 bytes
        assert_eq!(drv.transmit_apdu(&[0x00], &mut tiny_rsp), Err(Error::BufferTooSmall));
    }

    #[test]
    fn exchange_failure_when_link_lost() {
        let mut t = MockTransport::new();
        script_discover_select_activate(&mut t);
        // no DATA reply scripted

        let mut drv = Pn7160Driver::new(t);
        let mut atr = [0u8; 32];
        drv.power_on(&mut atr).expect("power_on");

        let mut rsp = [0u8; 256];
        assert_eq!(drv.transmit_apdu(&[0x00], &mut rsp), Err(Error::ExchangeFailed));
    }
}
