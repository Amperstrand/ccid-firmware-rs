//! PN7160 NFC driver: NfcDriver-shaped session management over any
//! `Transport`. All protocol logic lives in the reader/bring-up modules;
//! this layer manages session state and buffer handling. Host-testable
//! via the mock Transport; the firmware crate wraps it in a thin
//! `NfcDriver` delegation.

use super::{reader, Transport};
use reader::DiscoverNtf;

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
    uid: [u8; 10],
    uid_len: usize,
    /// Last discovery notification (edge-triggered chips report a tag
    /// ONCE on arrival): presence polls refresh it, activation reuses it.
    last_ntf: Option<DiscoverNtf>,
    /// #88 TTL re-arm bookkeeping: polls since the last discovery
    /// notification, and consecutive re-arm cycles that saw no tag.
    polls_since_ntf: u32,
    absent_rearms: u8,
}

/// Presence re-arm cadence in polls (issue #88): at the firmware's 500 ms
/// card-poll interval this re-arms discovery every ~4 s.
const PRESENCE_REARM_POLLS: u32 = 8;

/// Consecutive empty re-arm cycles before presence clears — a discovery
/// NTF can arrive one poll after the re-arm (TOTAL_DURATION latency) and
/// must not flap presence to absent.
const ABSENT_REARMS_TO_CLEAR: u8 = 2;

impl<T: Transport> Pn7160Driver<T> {
    pub fn new(transport: T) -> Self {
        Pn7160Driver {
            transport,
            active: false,
            uid: [0u8; 10],
            uid_len: 0,
            last_ntf: None,
            polls_since_ntf: 0,
            absent_rearms: 0,
        }
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
    /// Issue #88: the NFCC is edge-triggered — a tag placed AFTER the last
    /// notification never produces a new one, and a removed tag leaves the
    /// cache stale-present. After `PRESENCE_REARM_POLLS` quiet polls the
    /// driver re-arms discovery; `ABSENT_REARMS_TO_CLEAR` consecutive empty
    /// re-arms confirm absence.
    ///
    /// NCI_SPEC (v2.3 §5.2.5, RF_DEACTIVATE): deactivation with type IDLE
    /// returns the NFCC to the IDLE state, from which the discovery
    /// process (§5.1) restarts automatically — one fresh RF_DISCOVER_NTF
    /// (§4.4.2) is emitted per technology detected in the new cycle. The
    /// NTF is edge-triggered per ARRIVAL only while discovery CONTINUES to
    /// run; restarting the cycle is the only re-report mechanism.
    ///
    /// Timing basis: TOTAL_DURATION (CORE_SET_CONFIG TLV 0x0202, set in
    /// the init ladder to 510 ms) bounds the discovery cycle, so a re-arm
    /// NTF can land up to ~0.5 s after the deactivate response — hence
    /// the two-cycle absence confirmation instead of clearing on the
    /// first empty re-arm.
    pub fn is_card_present(&mut self) -> bool {
        if self.active {
            return true;
        }
        // TTL check first: the re-arm's own discovery read then gets first
        // access to frames arriving in response to the deactivate cycle
        // (a discovery read BEFORE the re-arm would consume them as plain
        // pending notifications instead).
        //
        // The re-arm cadence is UNCONDITIONAL (bench 2026-10-10): gating it
        // on `last_ntf.is_some()` left a driver whose boot-time discovery
        // NTF was consumed by the bring-up reads permanently inert — no
        // cached NTF, no re-arm, no fresh discovery, presence stuck absent
        // with the tag sitting on the coil. Running the cadence regardless
        // means an empty field cycles deactivate+discover (the deployed
        // negative-case behaviour) and a boot-present tag is re-discovered
        // within one cycle (~4 s).
        self.polls_since_ntf = self.polls_since_ntf.saturating_add(1);
        if self.polls_since_ntf >= PRESENCE_REARM_POLLS {
            self.polls_since_ntf = 0;
            self.rearm_discovery();
        }
        if let Some(n) = reader::wait_for_discovery(&mut self.transport) {
            self.last_ntf = Some(n);
            self.polls_since_ntf = 0;
            self.absent_rearms = 0;
        }
        self.last_ntf.is_some()
    }

    fn rearm_discovery(&mut self) {
        if reader::deactivate_idle(&mut self.transport).is_err() {
            // Transport-level failure: keep the last known presence.
            return;
        }
        match reader::wait_for_discovery(&mut self.transport) {
            Some(n) => {
                self.last_ntf = Some(n);
                self.absent_rearms = 0;
            }
            None => {
                self.absent_rearms = self.absent_rearms.saturating_add(1);
                if self.absent_rearms >= ABSENT_REARMS_TO_CLEAR {
                    self.last_ntf = None;
                    self.absent_rearms = 0;
                }
            }
        }
    }

    /// Discover, select, and activate a tag; copies the ATS (from the
    /// activation notification's Initial_Params — NCI §6.3.4) into `atr`.
    pub fn power_on(&mut self, atr: &mut [u8]) -> Result<usize, Error> {
        let ntf = reader::wait_for_discovery(&mut self.transport)
            .or(self.last_ntf)
            .ok_or(Error::NoCard)?;
        if let Some(uid) = reader::nfca_uid(&ntf) {
            self.uid_len = uid.len().min(10);
            self.uid[..self.uid_len].copy_from_slice(&uid[..self.uid_len]);
        }
        let activation = reader::select_tag(&mut self.transport, &ntf).map_err(Error::Select)?;
        let ats =
            reader::extract_ats(&activation).ok_or(Error::Select("activation carries no ATS"))?;
        // Construct a PC/SC-compatible ATR from the ATS (proper TS byte,
        // no TL/CRC_A) per PC/SC Part 3 contactless ATR rules.
        let atr_bytes = crate::transport::ats_to_atr(ats)
            .ok_or(Error::Select("ATS too short for ATR construction"))?;
        if atr.len() < atr_bytes.len() {
            return Err(Error::BufferTooSmall);
        }
        atr[..atr_bytes.len()].copy_from_slice(&atr_bytes);
        self.active = true;
        Ok(atr_bytes.len())
    }

    /// Deactivate the RF interface back to idle.
    pub fn power_off(&mut self) {
        let _ = reader::deactivate_idle(&mut self.transport);
        self.active = false;
        self.uid_len = 0;
        self.last_ntf = None;
    }

    /// Exchange one APDU with the activated tag (connection 0).
    pub fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Error> {
        if !self.active {
            return Err(Error::NoCard);
        }
        let rsp = reader::exchange(&mut self.transport, 0, command).ok_or(Error::ExchangeFailed)?;
        if response.len() < rsp.len() {
            return Err(Error::BufferTooSmall);
        }
        response[..rsp.len()].copy_from_slice(&rsp);
        Ok(rsp.len())
    }

    pub fn session_active(&self) -> bool {
        self.active
    }

    /// NFC-A UID from the most recent power_on discovery (empty if not ISO-14443-A).
    pub fn uid(&self) -> &[u8] {
        &self.uid[..self.uid_len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockTransport;
    use crate::{
        DEACTIVATE_TYPE_IDLE, GID_CORE, GID_RF, MT_NTF, MT_RSP, NCI_INTERFACE_ISO_DEP,
        NCI_PROTOCOL_ISO_DEP, NTF_RF_DEACTIVATE, NTF_RF_DISCOVER, NTF_RF_INTF_ACTIVATED,
        OID_CORE_INIT, OID_CORE_RESET, OID_CORE_SET_CONFIG, OID_RF_DEACTIVATE, OID_RF_DISCOVER,
        OID_RF_DISCOVER_MAP, OID_RF_DISCOVER_SELECT, STATUS_OK,
    };

    // A realistic ISO-DEP ATS (per ISO 14443-4: TL, T0, TA1, TB1)
    // Proper ATS: TL=5, T0=0x75, TA1=0x77, TB1=0x81, TC1=0x02
    const TEST_ATS: [u8; 5] = [0x05, 0x75, 0x77, 0x81, 0x02];
    // PC/SC ATR constructed from the ATS: 0x3B replaces TL, body follows
    const TEST_ATR: [u8; 5] = [0x3B, 0x75, 0x77, 0x81, 0x02];

    fn script_ladder(t: &mut MockTransport) {
        t.push_reply(&[MT_RSP | GID_CORE, OID_CORE_RESET, 0x01, STATUS_OK]);
        t.push_notification(&[MT_NTF, OID_CORE_RESET, 0x01, 0x00]);
        t.push_reply(&[MT_RSP | GID_CORE, OID_CORE_INIT, 0x01, STATUS_OK]);
        t.push_reply(&[
            MT_RSP | GID_CORE,
            OID_CORE_SET_CONFIG,
            0x02,
            0x01,
            STATUS_OK,
        ]);
        t.push_reply(&[
            MT_RSP | GID_CORE,
            OID_CORE_SET_CONFIG,
            0x02,
            0x01,
            STATUS_OK,
        ]);
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER_MAP, 0x01, STATUS_OK]);
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER, 0x01, STATUS_OK]);
    }

    fn script_discover_select_activate(t: &mut MockTransport) {
        // DISCOVER_NTF: [disc_id, proto, tech, params_len=0, interface]
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DISCOVER,
            0x05,
            0x01,
            NCI_PROTOCOL_ISO_DEP,
            0x00,
            0x00,
            NCI_INTERFACE_ISO_DEP,
        ]);
        // SELECT RSP
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER_SELECT, 0x01, STATUS_OK]);
        // INTF_ACTIVATED NTF with ATS in Initial_Params (NCI §6.3.4):
        // [id, intf, proto, tech, max_payload, params_len, ...ATS]
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_INTF_ACTIVATED,
            0x0B,
            0x01,
            NCI_INTERFACE_ISO_DEP,
            NCI_PROTOCOL_ISO_DEP,
            0x00,
            0xFF,
            TEST_ATS.len() as u8,
            TEST_ATS[0],
            TEST_ATS[1],
            TEST_ATS[2],
            TEST_ATS[3],
            TEST_ATS[4],
        ]);
    }

    fn script_apdu(t: &mut MockTransport, response: &[u8]) {
        let mut data = heapless::Vec::<u8, 258>::new();
        let _ = data.extend_from_slice(&[0x00, 0x00, response.len() as u8]);
        let _ = data.extend_from_slice(response);
        t.push_reply(&data);
    }

    fn script_deactivate(t: &mut MockTransport) {
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DEACTIVATE, 0x01, STATUS_OK]);
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x01,
            DEACTIVATE_TYPE_IDLE,
        ]);
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
        assert_eq!(&atr[..atr_len], &TEST_ATR);
        assert!(drv.session_active());

        let mut rsp = [0u8; 256];
        let rsp_len = drv
            .transmit_apdu(&[0x00, 0xA4, 0x04, 0x00], &mut rsp)
            .expect("apdu");
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
        assert_eq!(
            drv.transmit_apdu(&[0x00], &mut tiny_rsp),
            Err(Error::BufferTooSmall)
        );
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
        assert_eq!(
            drv.transmit_apdu(&[0x00], &mut rsp),
            Err(Error::ExchangeFailed)
        );
    }

    // --- Issue #88: TTL presence re-arm (edge-triggered discovery NTFs) ---

    fn discover_ntf() -> [u8; 8] {
        [
            MT_NTF | GID_RF,
            NTF_RF_DISCOVER,
            0x05,
            0x01,
            NCI_PROTOCOL_ISO_DEP,
            0x00,
            0x00,
            NCI_INTERFACE_ISO_DEP,
        ]
    }

    #[test]
    fn card_arriving_after_boot_is_found_via_rearm() {
        let mut drv = Pn7160Driver::new(MockTransport::new());
        // No NTF at boot — the card is placed into the field later. With
        // no cached notification the re-arm path stays dormant, so the
        // early polls are plain (and stay absent).
        for _ in 0..PRESENCE_REARM_POLLS - 1 {
            assert!(!drv.is_card_present());
        }
        // With last_ntf None the plain discovery read still finds a
        // late-arriving NTF within one poll (the NOT-yet-cached
        // direction; the cached direction is covered by
        // late_ntf_after_rearm_restores_presence_without_clear).
        let t = drv.transport_mut();
        t.push_notification(&discover_ntf());
        assert!(drv.is_card_present());
    }

    // given the boot-time discovery NTF was consumed by the bring-up reads
    // (never latched into last_ntf), when the unconditional re-arm cycle
    // restarts discovery, then the still-present tag is re-discovered
    // (bench regression, nucula 2026-10-10: tag on coil at boot, presence
    // stuck absent forever, Escape 0xD0 card_present=0).
    #[test]
    fn boot_ntf_eaten_by_bringup_rediscovered_by_rearm() {
        let mut drv = Pn7160Driver::new(MockTransport::new());
        for _ in 0..PRESENCE_REARM_POLLS - 1 {
            assert!(!drv.is_card_present(), "no NTF latched yet");
        }
        let t = drv.transport_mut();
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DEACTIVATE, 0x01, STATUS_OK]);
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x01,
            DEACTIVATE_TYPE_IDLE,
        ]);
        t.push_notification(&discover_ntf());
        assert!(drv.is_card_present(), "re-arm must rediscover the boot tag");
        assert!(drv.is_card_present(), "and stay present");
    }

    #[test]
    fn removed_card_clears_after_two_empty_rearms() {
        let mut t = MockTransport::new();
        t.push_notification(&discover_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());

        // Script frames per-poll (a pre-queued deactivate NTF would be
        // drained by the next poll's discovery read before the re-arm
        // needs it — on hardware these frames only arrive IN RESPONSE to
        // the deactivate command).
        for _ in 0..PRESENCE_REARM_POLLS - 1 {
            assert!(drv.is_card_present());
        }
        // Poll 8 = re-arm 1: deactivate RSP + NTF, discovery stays empty.
        let t = drv.transport_mut();
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DEACTIVATE, 0x01, STATUS_OK]);
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x01,
            DEACTIVATE_TYPE_IDLE,
        ]);
        assert!(drv.is_card_present(), "one empty re-arm must not clear");

        for _ in 0..PRESENCE_REARM_POLLS - 1 {
            assert!(drv.is_card_present());
        }
        // Poll 16 = re-arm 2: still no tag → presence clears exactly here.
        let t = drv.transport_mut();
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DEACTIVATE, 0x01, STATUS_OK]);
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x01,
            DEACTIVATE_TYPE_IDLE,
        ]);
        assert!(!drv.is_card_present(), "second empty re-arm must clear");
        // And it stays absent (no cached NTF → no re-arm path).
        assert!(!drv.is_card_present());
    }

    #[test]
    fn late_ntf_after_rearm_restores_presence_without_clear() {
        let mut t = MockTransport::new();
        t.push_notification(&discover_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());

        for _ in 0..PRESENCE_REARM_POLLS - 1 {
            assert!(drv.is_card_present());
        }
        // Re-arm 1: the deactivate completes but the discovery NTF has
        // not arrived yet (TOTAL_DURATION latency on real hardware) —
        // the re-arm's own read is empty, absent_rearms=1, no clear.
        let t = drv.transport_mut();
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DEACTIVATE, 0x01, STATUS_OK]);
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x01,
            DEACTIVATE_TYPE_IDLE,
        ]);
        assert!(drv.is_card_present(), "late NTF must not flap presence");

        // The discovery NTF lands during the next window's plain read:
        drv.transport_mut().push_notification(&discover_ntf());
        assert!(drv.is_card_present());
    }

    #[test]
    fn active_session_presence_short_circuits() {
        let mut t = MockTransport::new();
        script_discover_select_activate(&mut t);
        let mut drv = Pn7160Driver::new(t);
        let mut atr = [0u8; 32];
        drv.power_on(&mut atr).expect("power_on");

        // Many polls must not touch the transport (no re-arm during a
        // session — it would tear the RF interface down).
        let sent_before = drv.transport_mut().sent.len();
        for _ in 0..(PRESENCE_REARM_POLLS * 3) {
            assert!(drv.is_card_present());
        }
        assert_eq!(drv.transport_mut().sent.len(), sent_before);
    }
}
