//! PN7160 NFC driver: NfcDriver-shaped session management over any
//! `Transport`. All protocol logic lives in the reader/bring-up modules;
//! this layer manages session state and buffer handling. Host-testable
//! via the mock Transport; the firmware crate wraps it in a thin
//! `NfcDriver` delegation.

use super::{reader, Transport, STATUS_OK};
use reader::DiscoverNtf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    BringUp(&'static str),
    Select(&'static str),
    NoCard,
    ExchangeFailed,
    BufferTooSmall,
}

/// One step of the NON-BLOCKING presence re-arm (2026-10-10 serve-loop
/// stall fix). NCI v2.3 §5.2.5: the DH sends RF_DEACTIVATE(type=
/// DISCOVERY) and the NFCC answers RF_DEACTIVATE_RSP possibly only after
/// finishing its current RF activity — waiting for that RSP inline is
/// what stalled the CCID serve loop for ~2 s every re-arm cadence. The
/// machine splits the exchange across presence polls: ONE write on the
/// poll that enters the phase, ONE frame read per poll after, zero
/// blocking waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RearmPhase {
    /// Deactivate written, RSP not collected yet. `polls` counts polls
    /// spent waiting; the phase gives up (keeping last known presence)
    /// after [`REARM_RSP_POLLS`].
    AwaitRsp { polls: u8 },
    /// RSP was STATUS_OK — discovery restarted natively (NCI v2.3
    /// §5.2.5: type DISCOVERY returns the NFCC to the discovery loop; no
    /// RF_DISCOVER re-issue, which mutes the PN7160 after ~11 cycles,
    /// bench 2026-10-10). Tag NTFs are expected within TOTAL_DURATION
    /// (~0.5 s, CORE_SET_CONFIG TLV 0x0202) and land via the normal
    /// per-poll event drains.
    AwaitNtf { polls: u8 },
}

pub struct Pn7160Driver<T: Transport> {
    transport: T,
    active: bool,
    uid: [u8; 10],
    uid_len: usize,
    /// Last discovery notification (edge-triggered chips report a tag
    /// ONCE on arrival): presence polls refresh it, activation reuses it.
    last_ntf: Option<DiscoverNtf>,
    /// ATS carried by the last AUTO-ACTIVATION NTF (issue #88), kept in
    /// lockstep with `last_ntf`: when the chip already activated the
    /// tag, `power_on` builds the ATR from it and must NOT select (a
    /// select on an activated interface wedges the PN7160 — bench
    /// 2026-10-10). `None` for plain DISCOVER-NTF tags (ATS arrives
    /// with the select response there).
    last_auto_ats: Option<heapless::Vec<u8, 32>>,
    /// #88 TTL re-arm bookkeeping: polls since the last discovery
    /// notification, and consecutive re-arm cycles that saw no tag.
    polls_since_ntf: u32,
    absent_rearms: u8,
    /// In-flight split re-arm step (see [`RearmPhase`]).
    rearm: Option<RearmPhase>,
}

/// Presence re-arm cadence in polls (issue #88): at the firmware's 500 ms
/// card-poll interval this re-arms discovery every ~4 s.
const PRESENCE_REARM_POLLS: u32 = 8;

/// Consecutive empty re-arm cycles before presence clears — a discovery
/// NTF can arrive one poll after the re-arm (TOTAL_DURATION latency) and
/// must not flap presence to absent.
const ABSENT_REARMS_TO_CLEAR: u8 = 2;

/// Polls granted for the RF_DEACTIVATE_RSP to arrive (NCI v2.3 §5.2.5 —
/// the NFCC may finish its current RF activity first). At the 500 ms
/// presence interval this tolerates a ~1 s RSP delay; after that the
/// re-arm is abandoned, keeping the last known presence (same contract
/// as a transport failure in the old blocking path).
const REARM_RSP_POLLS: u8 = 2;

/// Polls of tag-collect window after a STATUS_OK RSP. Discovery
/// restarts natively (NCI v2.3 §5.2.5, type DISCOVERY) and a tag NTF
/// arrives within TOTAL_DURATION (~510 ms), so two 500 ms polls cover
/// it with margin; expiry counts as one empty re-arm cycle toward
/// [`ABSENT_REARMS_TO_CLEAR`].
const REARM_NTF_POLLS: u8 = 2;

impl<T: Transport> Pn7160Driver<T> {
    pub fn new(transport: T) -> Self {
        Pn7160Driver {
            transport,
            active: false,
            uid: [0u8; 10],
            uid_len: 0,
            last_ntf: None,
            last_auto_ats: None,
            polls_since_ntf: 0,
            absent_rearms: 0,
            rearm: None,
        }
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    /// In-flight split re-arm step, for diagnostics and tests.
    pub fn rearm_phase(&self) -> Option<RearmPhase> {
        self.rearm
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
    /// returns the NFCC to the IDLE state. BENCH CORRECTION (2026-10-10,
    /// issue #105): the PN7160 does NOT auto-restart discovery from
    /// IDLE — the re-arm issues RF_DEACTIVATE(type=DISCOVERY), which
    /// tears down the RF interface AND returns the NFCC to the discovery
    /// loop in one command; a tag still in the field answers with a fresh
    /// NTF (DISCOVER, or the INTF_ACTIVATED of an auto-activated ISO-DEP
    /// tag) up to TOTAL_DURATION (~0.5 s) later.
    ///
    /// NON-BLOCKING (2026-10-10 serve-loop stall fix): the re-arm is a
    /// state machine advanced one bounded step per call — the entering
    /// poll performs exactly ONE transport write (the deactivate), each
    /// later poll reads at most ONE pending frame or drains already-
    /// pending notifications, and no step ever waits for the chip. The
    /// bench regression this fixes: ~2 s GetSlotStatus stalls at the
    /// ~5.7 s re-arm cadence (1414-probe histogram, 2026-10-10), which
    /// retired the reader from pcscd (EHStatusHandlerThread).
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
        self.step_rearm();
        match reader::wait_for_event(&mut self.transport) {
            Some(reader::TagEvent::Tag(n, ats)) => {
                self.latch_tag(n, ats);
                return true;
            }
            // RF_DEACTIVATE_NTF(reason = RF link lost, NCI §5.2.5.2):
            // the auto-activated tag left the field — clear immediately.
            // (Host-requested deactivations carry reason DH-request and
            // are filtered out in `event_of`.)
            Some(reader::TagEvent::LinkLost) => {
                self.clear_tag();
                return false;
            }
            None => {}
        }
        if self.last_auto_ats.is_some() {
            // Live auto-activation: the tag sits IN the active RF
            // interface — presence is physical fact, and removal
            // announces itself (LinkLost above). NO re-arm cycling in
            // this state: repeatedly deactivating an auto-activated
            // interface wedges the PN7160 after ~10-11 cycles (bench
            // 2026-10-10: clean cycles for ~44 s, then I2C NACKs until
            // power cycle), and the cycles yield no fresh NTFs anyway.
            return true;
        }
        if self.rearm.is_none() {
            self.polls_since_ntf = self.polls_since_ntf.saturating_add(1);
            if self.polls_since_ntf >= PRESENCE_REARM_POLLS {
                self.polls_since_ntf = 0;
                if self.transport.send(&reader::RF_DEACTIVATE_DISCOVERY) {
                    self.rearm = Some(RearmPhase::AwaitRsp { polls: 0 });
                }
                // Write failure (mute link): keep the last known
                // presence and retry after a full cadence.
            }
        }
        self.last_ntf.is_some()
    }

    /// Advance the split re-arm by one poll-bounded step (see
    /// [`RearmPhase`]). At most one `poll_frame` read here; the one
    /// write happened on the poll that entered the phase.
    fn step_rearm(&mut self) {
        let Some(phase) = self.rearm else { return };
        match phase {
            RearmPhase::AwaitRsp { polls } => {
                match self.transport.poll_frame() {
                    Some(f) if f.is_rsp_to(&reader::RF_DEACTIVATE_DISCOVERY) => {
                        self.rearm = if f.status() == Some(STATUS_OK) {
                            Some(RearmPhase::AwaitNtf { polls: 0 })
                        } else {
                            // NFCC refused the re-arm: keep last known
                            // presence, retry after a full cadence.
                            None
                        };
                    }
                    Some(f) => match reader::event_of(&f) {
                        // Presence signal: applied and the phase exits
                        // with it (absorb clears `rearm`).
                        Some(ev) => self.absorb_event(ev),
                        // Presence-neutral frame (credits, DH-requested
                        // deactivate NTF): stay in phase, deadline still
                        // advances.
                        None => self.advance_or_give_up(polls),
                    },
                    None => self.advance_or_give_up(polls),
                }
            }
            RearmPhase::AwaitNtf { polls } => {
                if polls + 1 >= REARM_NTF_POLLS {
                    // Collect window expired: TOTAL_DURATION has lapsed
                    // with no tag NTF — one empty re-arm cycle.
                    self.rearm = None;
                    self.polls_since_ntf = 0;
                    self.absent_rearms = self.absent_rearms.saturating_add(1);
                    if self.absent_rearms >= ABSENT_REARMS_TO_CLEAR {
                        self.last_ntf = None;
                        self.last_auto_ats = None;
                        self.absent_rearms = 0;
                    }
                } else {
                    self.rearm = Some(RearmPhase::AwaitNtf { polls: polls + 1 });
                }
            }
        }
    }

    fn advance_or_give_up(&mut self, polls: u8) {
        self.rearm = if polls + 1 >= REARM_RSP_POLLS {
            // RSP never arrived within the granted polls (NCI v2.3
            // §5.2.5 allows the NFCC to defer it behind RF activity):
            // abandon the re-arm, keep last known presence.
            None
        } else {
            Some(RearmPhase::AwaitRsp { polls: polls + 1 })
        };
    }

    /// Apply a presence signal read OUTSIDE the normal drain path (the
    /// re-arm's `poll_frame`); exits any in-flight phase.
    fn absorb_event(&mut self, ev: reader::TagEvent) {
        match ev {
            reader::TagEvent::Tag(n, ats) => self.latch_tag(n, ats),
            reader::TagEvent::LinkLost => self.clear_tag(),
        }
    }

    fn latch_tag(&mut self, n: DiscoverNtf, ats: Option<heapless::Vec<u8, 32>>) {
        self.last_ntf = Some(n);
        self.last_auto_ats = ats;
        self.polls_since_ntf = 0;
        self.absent_rearms = 0;
        self.rearm = None;
    }

    fn clear_tag(&mut self) {
        self.last_ntf = None;
        self.last_auto_ats = None;
        self.polls_since_ntf = 0;
        self.absent_rearms = 0;
        self.rearm = None;
    }

    /// Discover, select, and activate a tag; copies the ATS (from the
    /// activation notification's Initial_Params — NCI §6.3.4) into `atr`.
    ///
    /// Auto-activated tags (issue #88): when the PN7160 has already
    /// activated the tag on its own, the RF interface is UP and
    /// `RF_DISCOVER_SELECT` must NOT be sent — bench 2026-10-10:
    /// selecting an already-activated interface wedges the chip (I2C
    /// NACKs, RSP timeouts until power cycle). In that flow the ATS
    /// comes from the activation NTF itself (its tail TLV) and the
    /// established connection serves APDUs directly.
    pub fn power_on(&mut self, atr: &mut [u8]) -> Result<usize, Error> {
        let (fresh_ntf, fresh_ats) = match reader::wait_for_tag(&mut self.transport) {
            Some((n, ats)) => {
                self.last_ntf = Some(n);
                self.last_auto_ats = ats.clone();
                (Some(n), ats)
            }
            None => (None, None),
        };
        let ntf = fresh_ntf.or(self.last_ntf).ok_or(Error::NoCard)?;
        let auto_ats = fresh_ats.or_else(|| self.last_auto_ats.clone());
        if let Some(uid) = reader::nfca_uid(&ntf) {
            self.uid_len = uid.len().min(10);
            self.uid[..self.uid_len].copy_from_slice(&uid[..self.uid_len]);
        }
        let ats: heapless::Vec<u8, 32> = match auto_ats {
            Some(ats) => ats,
            None => {
                let activation =
                    reader::select_tag(&mut self.transport, &ntf).map_err(Error::Select)?;
                let extracted = reader::extract_ats(&activation)
                    .ok_or(Error::Select("activation carries no ATS"))?;
                let mut v = heapless::Vec::new();
                v.extend_from_slice(extracted)
                    .map_err(|_| Error::Select("ATS too long"))?;
                v
            }
        };
        // Construct a PC/SC-compatible ATR from the ATS (proper TS byte,
        // no TL/CRC_A) per PC/SC Part 3 contactless ATR rules.
        let atr_bytes = crate::transport::ats_to_atr(&ats)
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
        self.last_auto_ats = None;
        self.rearm = None;
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
        DEACTIVATE_TYPE_IDLE, GID_CORE, GID_RF, MT_CMD, MT_NTF, MT_RSP, NCI_INTERFACE_ISO_DEP,
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

    /// The PN7160's auto-activation RF_INTF_ACTIVATED NTF, byte-for-byte
    /// as captured on the nucula bench (2026-10-10, issue #88; ISO-DEP
    /// card on the coil): [disc=1, intf=ISO_DEP, proto=ISO_DEP,
    /// tech=NFC-A poll, max=0xFF, TLV(0x01, 13: ATQA 0x0048, UID
    /// 04 39 80 8A AE 17 90, …), 0,0,0, TLV(0x0A, ATS
    /// 09 78 77 91 02 80 73 C8 21)].
    fn auto_activation_ntf() -> [u8; 37] {
        [
            97, 5, 34, 1, 2, 4, 0, 255, 1, 13, 72, 0, 7, 4, 57, 128, 138, 174, 23, 144, 1, 32, 0,
            0, 0, 0, 10, 9, 120, 119, 145, 2, 128, 115, 200, 33, 16,
        ]
    }

    // given the chip AUTO-ACTIVATES the ISO-DEP tag during discovery (no
    // RF_DISCOVER_NTF ever emitted — issue #88 bench evidence), when the
    // activation NTF arrives, then presence reports the tag.
    #[test]
    fn auto_activation_ntf_counts_as_presence() {
        let mut t = MockTransport::new();
        t.push_notification(&auto_activation_ntf());
        let mut drv = Pn7160Driver::new(t);

        assert!(drv.is_card_present());
    }

    // given the tag is ALREADY activated (auto-activation), when the host
    // powers on, then NO RF_DISCOVER_SELECT is sent (it wedges the
    // PN7160 — bench 2026-10-10), the ATR comes from the activation
    // NTF's ATS TLV, and the session serves APDUs over the established
    // connection.
    #[test]
    fn power_on_auto_activated_tag_skips_select_and_uses_ntf_ats() {
        let mut t = MockTransport::new();
        t.push_notification(&auto_activation_ntf());
        // APDU reply rides the already-established connection 0.
        let mut apdu_rsp = heapless::Vec::<u8, 258>::new();
        let _ = apdu_rsp.extend_from_slice(&[0x00, 0x00, 0x02, 0x90, 0x00]);
        t.push_reply(&apdu_rsp);

        let mut drv = Pn7160Driver::new(t);
        assert!(
            drv.is_card_present(),
            "presence consumes the activation NTF"
        );

        let mut atr = [0u8; 32];
        let atr_len = drv
            .power_on(&mut atr)
            .expect("power_on via auto-activation");
        assert_eq!(
            &atr[..atr_len],
            &[0x3B, 0x78, 0x77, 0x91, 0x02, 0x80, 0x73, 0xC8, 0x21],
            "ATR from the cached activation ATS"
        );
        assert_eq!(
            drv.uid(),
            [0x04, 0x39, 0x80, 0x8A, 0xAE, 0x17, 0x90],
            "UID from the activation NTF's tech params"
        );
        assert!(drv.session_active());

        let mut rsp = [0u8; 256];
        let n = drv.transmit_apdu(&[0x00, 0xA4], &mut rsp).expect("apdu");
        assert_eq!(&rsp[..n], &[0x90, 0x00]);

        // The wire never saw an RF_DISCOVER_SELECT.
        let recovered = drv.into_transport();
        assert!(
            !recovered
                .sent
                .iter()
                .any(|f| f.len() >= 2 && f[0] == MT_CMD | GID_RF && f[1] == OID_RF_DISCOVER_SELECT),
            "select must not be attempted on an auto-activated tag"
        );
    }

    // given a live auto-activation, when presence is polled many times,
    // then NO re-arm cycles run (deactivating an auto-activated
    // interface wedges the PN7160 — bench 2026-10-10) and the tag stays
    // present.
    #[test]
    fn auto_presence_survives_many_polls_without_cycling() {
        let mut t = MockTransport::new();
        t.push_notification(&auto_activation_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());

        let sent_when_cached = drv.transport_mut().sent.len();
        for i in 0..(PRESENCE_REARM_POLLS * 3) {
            assert!(drv.is_card_present(), "poll {}", i);
        }
        assert_eq!(
            drv.transport_mut().sent.len(),
            sent_when_cached,
            "live activation must not trigger re-arm cycles"
        );
    }

    // given an auto-activated tag, when the chip reports RF_DEACTIVATE
    // with reason RF-link-lost (the tag left the field), then presence
    // clears immediately; a DH-requested deactivation (our own
    // teardown) must not be mistaken for removal.
    #[test]
    fn rf_link_lost_clears_auto_presence() {
        let mut t = MockTransport::new();
        t.push_notification(&auto_activation_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());

        // reason 0x01 = DH request (e.g. a host power_off in flight) —
        // not a removal; presence unchanged.
        drv.transport_mut().push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x02,
            DEACTIVATE_TYPE_IDLE,
            0x01,
        ]);
        assert!(drv.is_card_present(), "DH-requested deactivate ≠ removal");

        // reason 0x00 = RF link lost — the tag is gone.
        drv.transport_mut().push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x02,
            DEACTIVATE_TYPE_IDLE,
            0x00,
        ]);
        assert!(!drv.is_card_present(), "link loss = immediate absent");
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
        // Cadence poll: the deactivate is WRITTEN (RSP scripted for it);
        // the tag NTFs only arrive on the wires afterwards, like the
        // chip's post-deactivate discovery restart on hardware.
        drv.transport_mut()
            .push_reply(&deactivate_discovery_rsp_ok());
        assert!(!drv.is_card_present());
        assert_eq!(
            drv.rearm_phase(),
            Some(RearmPhase::AwaitRsp { polls: 0 }),
            "cadence poll wrote the deactivate"
        );
        let t = drv.transport_mut();
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x01,
            DEACTIVATE_TYPE_IDLE,
        ]);
        t.push_notification(&discover_ntf());
        // Next poll: RSP collected, then the plain event drain sees the
        // re-issued discovery's NTF — the boot tag is back.
        assert!(drv.is_card_present(), "re-arm must rediscover the boot tag");
        assert_eq!(drv.rearm_phase(), None);
        assert!(drv.is_card_present(), "and stay present");
    }

    fn deactivate_discovery_rsp_ok() -> [u8; 4] {
        [MT_RSP | GID_RF, OID_RF_DEACTIVATE, 0x01, STATUS_OK]
    }

    /// Advance polls (bounding each to the non-blocking contract) until
    /// one re-arm cycle completes: the deactivate was written and the
    /// phase machine returned to idle. `rsp` scripts the deactivate RSP.
    fn run_rearm_cycle(drv: &mut Pn7160Driver<MockTransport>, rsp: &[u8]) {
        drv.transport_mut().push_reply(rsp);
        let writes_before = drv.transport_mut().sent.len();
        for i in 0..(PRESENCE_REARM_POLLS as usize + 8) {
            let _ = drv.is_card_present();
            let wrote = drv.transport_mut().sent.len();
            let done = wrote > writes_before && drv.rearm_phase().is_none();
            if done {
                return;
            }
            assert!(
                wrote <= writes_before + 1,
                "poll {} wrote more than one frame",
                i
            );
        }
        panic!("re-arm cycle did not complete within the poll budget");
    }

    #[test]
    fn removed_card_clears_after_two_empty_rearms() {
        let mut t = MockTransport::new();
        t.push_notification(&discover_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());

        run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
        assert!(drv.is_card_present(), "one empty re-arm must not clear");

        run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
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
        // the collect window expires empty, absent_rearms=1, no clear.
        drv.transport_mut()
            .push_reply(&deactivate_discovery_rsp_ok());
        assert!(drv.is_card_present(), "late NTF must not flap presence");

        // The discovery NTF lands during the next window's plain read:
        drv.transport_mut().push_notification(&discover_ntf());
        assert!(drv.is_card_present());
    }

    // --- Non-blocking re-arm (2026-10-10 serve-loop stall fix) ---

    // given the ~2 s GetSlotStatus stalls were the re-arm's inline RSP
    // wait, when presence polls run, then NO poll ever performs a
    // blocking transact and at most ONE write happens per poll — the
    // exchange is spread across polls instead.
    #[test]
    fn presence_polls_never_transact_and_write_at_most_once() {
        let mut t = MockTransport::new();
        t.push_notification(&discover_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());

        for cycle in 0..3 {
            run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
            assert_eq!(
                drv.transport_mut().transacts,
                0,
                "cycle {}: presence path must never block on a transact",
                cycle
            );
        }
    }

    // given the NFCC defers the RF_DEACTIVATE_RSP behind its RF activity
    // (NCI v2.3 §5.2.5 — the bench's ~2 s stall), when the RSP is not on
    // the wire yet, then the write poll returns without waiting, the RSP
    // is collected on a later poll, and a permanently missing RSP gives
    // up at the deadline while keeping the last known presence.
    #[test]
    fn deferred_rsp_collected_later_and_missing_rsp_gives_up() {
        let mut t = MockTransport::new();
        t.push_notification(&discover_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());
        for _ in 0..PRESENCE_REARM_POLLS - 1 {
            assert!(drv.is_card_present());
        }
        // Cadence poll with NO scripted RSP: the write happens, nothing
        // is waited for.
        assert!(drv.is_card_present());
        assert_eq!(drv.rearm_phase(), Some(RearmPhase::AwaitRsp { polls: 0 }));

        // RSP lands before the deadline: collected, machine advances to
        // the tag-collect window.
        drv.transport_mut().pending_rsp = Some(deactivate_discovery_rsp_ok().to_vec());
        assert!(drv.is_card_present());
        assert_eq!(drv.rearm_phase(), Some(RearmPhase::AwaitNtf { polls: 0 }));

        // Window expires without a tag: one empty cycle, presence held.
        assert!(drv.is_card_present());
        assert!(drv.is_card_present());
        assert_eq!(drv.rearm_phase(), None);

        // Now the mute-link direction: RSP never arrives at all. Poll
        // until the cadence writes the next deactivate (the expiry poll
        // itself counts toward the next cadence).
        let mut wrote = false;
        for _ in 0..(PRESENCE_REARM_POLLS + 2) {
            assert!(drv.is_card_present());
            if drv.rearm_phase() == Some(RearmPhase::AwaitRsp { polls: 0 }) {
                wrote = true;
                break;
            }
        }
        assert!(wrote, "cadence must write a new deactivate");
        assert!(drv.is_card_present());
        assert_eq!(drv.rearm_phase(), Some(RearmPhase::AwaitRsp { polls: 1 }));
        assert!(drv.is_card_present());
        assert_eq!(
            drv.rearm_phase(),
            None,
            "deadline expired: re-arm abandoned"
        );
        assert!(
            drv.is_card_present(),
            "abandoned re-arm keeps last known presence"
        );
    }

    // given a tag arrives while a re-arm is mid-flight, when its NTF is
    // the frame poll_frame reads, then it is absorbed (presence flips,
    // phase exits) — the re-arm never masks a fresh sighting.
    #[test]
    fn tag_ntf_during_await_rsp_is_absorbed() {
        let mut t = MockTransport::new();
        let mut drv = Pn7160Driver::new(t);
        for _ in 0..PRESENCE_REARM_POLLS - 1 {
            assert!(!drv.is_card_present());
        }
        // Cadence poll writes the deactivate; no RSP scripted.
        assert!(!drv.is_card_present());
        assert_eq!(drv.rearm_phase(), Some(RearmPhase::AwaitRsp { polls: 0 }));
        // The next pending frame is the tag's NTF (discovery restarted
        // and found it immediately), not the RSP.
        drv.transport_mut().push_notification(&discover_ntf());
        assert!(drv.is_card_present());
        assert_eq!(drv.rearm_phase(), None);
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
