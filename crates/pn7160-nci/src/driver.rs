//! PN7160 NFC driver: NfcDriver-shaped session management over any
//! `Transport`. All protocol logic lives in the reader/bring-up modules;
//! this layer manages session state and buffer handling. Host-testable
//! via the mock Transport; the firmware crate wraps it in a thin
//! `NfcDriver` delegation.

use super::{reader, reader::ExchangeOutcome, Transport, STATUS_OK};
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
    /// bench 2026-10-10). `polls` counts the tag-collect window; expiry
    /// counts one empty re-arm cycle toward [`ABSENT_REARMS_TO_CLEAR`].
    AwaitNtf { polls: u8 },
    /// RF_DISCOVER probe written (the one-shot #125 recovery), RSP not
    /// collected yet. Same poll-bounded shape as [`RearmPhase::AwaitRsp`].
    DiscoverProbeRsp { polls: u8 },
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
    collect_rounds: u8,
    /// #88 TTL re-arm bookkeeping: polls since the last discovery
    /// notification, and consecutive re-arm cycles that saw no tag.
    polls_since_ntf: u32,
    absent_rearms: u8,
    /// In-flight split re-arm step (see [`RearmPhase`]).
    rearm: Option<RearmPhase>,
    /// One-shot RF_DISCOVER probe allowance (issue #125): armed on every
    /// presence/session edge (tag latch/clear, power_off), consumed by
    /// the first empty re-arm window. Keeps the recovery probe off the
    /// steady-state cadence — a per-cycle RF_DISCOVER re-issue mutes the
    /// PN7160 after ~11 cycles (bench 2026-10-10).
    probe_armed: bool,
    /// Park-in-discovery state (bench 2026-10-12): the DEACTIVATE re-arm
    /// cadence itself semi-mutes the PN7160 after ~11 cycles under WiFi
    /// load — I2C keeps ACKing but the NFCC stops answering RSPs. Once
    /// absence is confirmed the driver parks: the NFCC sits in perpetual
    /// discovery (its natural state), where an arriving tag
    /// auto-activates and raises its NTF without host cycling.
    discovery_parked: bool,
    /// Time of the last re-arm WRITE (ms domain of poll_maintenance);
    /// None while un-parked (the poll counter owns the cadence then).
    last_rearm_ms: Option<u64>,
    /// Re-arm writes ACKed but never answered (semi-mute signal): a
    /// healthy NFCC answers RF_DEACTIVATE in milliseconds; three
    /// silent re-arms mean the chip wedged — only a VEN power cycle
    /// recovers it.
    silent_rearms: u8,
    /// Stall hook invoked from inside a running DATA exchange (issue
    /// #156 B): receives the nominal elapsed collection wait in ms so
    /// the CCID layer can emit interim time-extension frames. `None`
    /// on host tests and any caller without a wire to notify.
    stall_hook: Option<reader::StallHook>,
    /// WTX advisories (`6F 17 00`) consumed during the LAST exchange
    /// (issue #156 A3) and the lifetime total — surfaced through
    /// Escape 0xD4. Bench model: one advisory per
    /// READER_FWITOX_NTF_CFG period (default 0x1000 × 4096/fc ≈ 1.24 s,
    /// UM11495 Rev 1.8 Table 99), so a stalled exchange's count is
    /// ≈ stall / 1.24 s.
    wtx_last: u32,
    wtx_total: u32,
}

/// Presence re-arm cadence in polls (issue #88): at the firmware's 500 ms
/// card-poll interval this re-arms discovery every ~4 s.
const PRESENCE_REARM_POLLS: u32 = 8;

/// Parked re-arm cadence (#326): after absence is confirmed the driver
/// still re-arms at this SLOW cadence instead of never — a parked NFCC
/// is idle and deaf, and on host-driven poll paths (vpcd: the host is
/// the only poller) "parked forever" wedges presence at absent. Slow
/// enough to stay far from the ~11-rapid-cycle chip-mute class (#125).
const PARKED_REARM_POLLS: u32 = 32;

/// #328/#332 (Codex P1): time-based parked re-arm — pcscd STOPS polling
/// entirely once it marks a reader dead, so a poll-count cadence can
/// never fire again. Equivalent of PARKED_REARM_POLLS at the ~400 ms
/// host probe cadence.
const PARKED_REARM_MS: u64 = 12_800;

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
            collect_rounds: 0,
            polls_since_ntf: 0,
            absent_rearms: 0,
            rearm: None,
            probe_armed: false,
            discovery_parked: false,
            last_rearm_ms: None,
            silent_rearms: 0,
            stall_hook: None,
            wtx_last: 0,
            wtx_total: 0,
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

    /// Consecutive ACKed-but-unanswered re-arm writes (semi-mute
    /// detector): the serve loop VEN-cycles the PN7160 on threshold.
    pub fn silent_rearm_streak(&self) -> u8 {
        self.silent_rearms
    }

    /// Install the exchange stall hook (issue #156 B). The hook runs
    /// deep inside a blocking DATA exchange — the only channel from
    /// there to the CCID wire — so it must be a plain function pointer
    /// reading whatever context it needs from statics.
    pub fn set_stall_hook(&mut self, hook: Option<reader::StallHook>) {
        self.stall_hook = hook;
    }

    /// WTX advisory counters (issue #156 A3): `(last exchange, total)`.
    pub fn wtx_counters(&self) -> (u32, u32) {
        (self.wtx_last, self.wtx_total)
    }
}

impl<T: Transport> Pn7160Driver<T> {
    /// NCI bring-up ladder (CORE_RESET → CORE_INIT → SET_CONFIG →
    /// DISCOVER_MAP → DISCOVER). Call once after VEN power-cycle.
    pub fn init(&mut self) -> Result<(), Error> {
        super::run_ladder(&mut self.transport).map_err(Error::BringUp)?;
        // Boot-NTF recovery (bench regression: a tag on the coil at boot
        // has its activation NTF consumed by the bring-up exchanges and
        // presence stays absent until the first re-arm — see
        // boot_ntf_eaten_by_bringup_rediscovered_by_rearm). Force the
        // FIRST presence poll to re-arm discovery immediately instead of
        // after PRESENCE_REARM_POLLS quiet polls.
        self.polls_since_ntf = PRESENCE_REARM_POLLS;
        Ok(())
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
            // Issue #109 item 2: a live session must not blind the
            // driver. The chip announces a mid-session removal as an
            // RF_DEACTIVATE_NTF (reason RF-link-loss — NCI §5.2.5.2,
            // Table 81); that frame surfaces via the plain drain (on the
            // I2C link, `transact` stashes NTFs that arrive mid-exchange),
            // and this poll turns it into the presence transition the
            // serve layer announces to the host. Strictly non-blocking
            // (drain only): no writes, no re-arm — an active RF interface
            // must not be torn down behind the session's back.
            match reader::wait_for_event(&mut self.transport) {
                Some(reader::TagEvent::LinkLost) => {
                    self.teardown_on_link_loss();
                    return false;
                }
                // A tag-shaped NTF while active (re-emitted activation)
                // just refreshes the cache; the session continues.
                Some(reader::TagEvent::Tag(n, ats)) => self.latch_tag(n, ats),
                None => {}
            }
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
        // #326 (2026-10-20): PARKED FOREVER IS A DEADLOCK on host-driven
        // poll paths. The park assumed "perpetual discovery sees the
        // next arrival" — but an NCI discovery cycle only polls for
        // TOTAL_DURATION (~510 ms) per re-arm; parked means the NFCC is
        // IDLE and deaf while `last_ntf` stays None → is_card_present()
        // answers false on every later poll, forever. On the serial
        // serve loop the 500 ms cadence papered over parking (re-arms
        // kept flowing); on vpcd the host is the ONLY poller, and
        // pcscd's eject/re-poll loop against a parked driver wedges
        // SCardConnect (the wifi-vpcd ATR regression). This also
        // reintroduced the #88 "tag placed after the last notification"
        // bug for host-driven paths. Fix: a parked driver still re-arms
        // at a SLOW cadence — far slower than the fast-cycle chip-mute
        // class (~11 rapid cycles, #125), so the link stays healthy —
        // so a card placed (or sitting) in the field is found within
        // one slow cycle.
        let cadence = if self.discovery_parked {
            PARKED_REARM_POLLS
        } else {
            PRESENCE_REARM_POLLS
        };
        if self.rearm.is_none() {
            self.polls_since_ntf = self.polls_since_ntf.saturating_add(1);
            if self.polls_since_ntf >= cadence {
                self.polls_since_ntf = 0;
                // Re-arm step 1 is ALWAYS the #105 one-command
                // RF_DEACTIVATE(type=DISCOVERY): from a running
                // discovery (the perpetual run_ladder path — the
                // NORMAL parked state, Codex P1 on #349) it tears the
                // interface down and returns to the discovery loop in
                // one command. From a TRULY idle NFCC (explicit
                // RF_DEACTIVATE(type=IDLE) earlier) it can be REFUSED —
                // the AwaitRsp handler then escalates to RF_DISCOVER
                // instead of retrying the refused command forever.
                if self.transport.send(&reader::RF_DEACTIVATE_DISCOVERY) {
                    self.rearm = Some(RearmPhase::AwaitRsp { polls: 0 });
                }
                // Write failure (mute link): keep the last known
                // presence and retry after a full cadence.
            }
        }
        self.last_ntf.is_some()
    }

    /// #328/#332 (Codex P1): time-driven maintenance for host-silent
    /// periods — pcscd stops polling once a reader is marked dead, so
    /// the parked re-arm must advance by TIME, not poll count. Called
    /// from the serve loop's idle wake (bounded, non-blocking: one
    /// state-machine step + at most one write when the window elapses).
    /// `now_ms` is the caller's monotonic clock (tests inject it
    /// deterministically).
    pub fn poll_maintenance(&mut self, now_ms: u64) {
        // Advance any in-flight re-arm phase one bounded step (RSP/NTF
        // collection) so the machine finishes even with no host polls.
        self.step_rearm();
        if !self.discovery_parked || self.rearm.is_some() {
            return;
        }
        let due = match self.last_rearm_ms {
            Some(last) => now_ms.saturating_sub(last) >= PARKED_REARM_MS,
            None => true, // parked but never re-armed by time — due now
        };
        if due && self.transport.send(&reader::RF_DEACTIVATE_DISCOVERY) {
            self.last_rearm_ms = Some(now_ms);
            self.rearm = Some(RearmPhase::AwaitRsp { polls: 0 });
        }
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
                        self.silent_rearms = 0;
                        self.rearm = if f.status() == Some(STATUS_OK) {
                            Some(RearmPhase::AwaitNtf { polls: 0 })
                        } else if self.transport.send(&reader::RF_DISCOVER_PASSIVE_A) {
                            // #349 (Codex): the deactivate was REFUSED —
                            // the NFCC is IDLE (nothing to deactivate;
                            // the perpetual-discovery parked state sends
                            // OK instead). Restart discovery directly:
                            // RF_DISCOVER, the #125 one-shot probe shape
                            // (rapid-cycle mute stays out of reach —
                            // this fires once per refused re-arm only).
                            Some(RearmPhase::DiscoverProbeRsp { polls: 0 })
                        } else {
                            // Refused AND the probe write failed (mute
                            // link): keep last known presence, retry
                            // after a full cadence.
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
                        // Park: absence confirmed — the cadence stops
                        // here; perpetual discovery sees the next arrival.
                        self.discovery_parked = true;
                    }
                    // #125 recovery: an empty window right after a
                    // presence/session edge may mean the NFCC sat down
                    // (session teardown stops polling on the bench
                    // PN7160 even with a card in the field). One
                    // non-blocking RF_DISCOVER probe, then a fresh NTF
                    // window — never per-cycle (chip mute, ~11 cycles).
                    if self.probe_armed && self.transport.send(&reader::RF_DISCOVER_PASSIVE_A) {
                        self.probe_armed = false;
                        self.rearm = Some(RearmPhase::DiscoverProbeRsp { polls: 0 });
                    }
                } else {
                    self.rearm = Some(RearmPhase::AwaitNtf { polls: polls + 1 });
                }
            }
            RearmPhase::DiscoverProbeRsp { polls } => match self.transport.poll_frame() {
                Some(f) if f.is_rsp_to(&reader::RF_DISCOVER_PASSIVE_A) => {
                    self.rearm = if f.status() == Some(STATUS_OK) {
                        Some(RearmPhase::AwaitNtf { polls: 0 })
                    } else {
                        None
                    };
                }
                Some(f) => match reader::event_of(&f) {
                    Some(ev) => self.absorb_event(ev),
                    None => self.advance_probe_or_give_up(polls),
                },
                None => self.advance_probe_or_give_up(polls),
            },
        }
    }

    fn advance_probe_or_give_up(&mut self, polls: u8) {
        self.rearm = if polls + 1 >= REARM_RSP_POLLS {
            // #349 (Codex): a probe whose RSP never arrives is a SILENT
            // re-arm — count it so the wifi VEN-cycle recovery's
            // silent-streak threshold can fire (matches AwaitRsp).
            self.silent_rearms = self.silent_rearms.saturating_add(1);
            None
        } else {
            Some(RearmPhase::DiscoverProbeRsp { polls: polls + 1 })
        };
    }

    fn advance_or_give_up(&mut self, polls: u8) {
        self.rearm = if polls + 1 >= REARM_RSP_POLLS {
            // RSP never arrived within the granted polls (NCI v2.3
            // §5.2.5 allows the NFCC to defer it behind RF activity):
            // abandon the re-arm, keep last known presence.
            self.silent_rearms = self.silent_rearms.saturating_add(1);
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
        self.probe_armed = true;
        self.discovery_parked = false;
        self.silent_rearms = 0;
    }

    fn clear_tag(&mut self) {
        self.last_ntf = None;
        self.last_auto_ats = None;
        self.polls_since_ntf = 0;
        self.absent_rearms = 0;
        self.rearm = None;
        self.probe_armed = true;
        self.discovery_parked = false;
    }

    /// Session teardown on mid-session link-loss (issue #109 item 2):
    /// the tag physically left the field while the CCID session was
    /// live. Clears EVERYTHING a later `power_on` could resurrect —
    /// the cached NTF and the auto-activation ATS included — so no
    /// stale activation answers for a card that is gone, and the
    /// session ends (`transmit_apdu` → NoCard) without a wire write:
    /// the chip already tore the RF interface down itself.
    fn teardown_on_link_loss(&mut self) {
        self.active = false;
        self.uid_len = 0;
        self.clear_tag();
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
        // #125 collect-wait: after a power_off the re-issued discovery
        // auto-activates the tag within ~1 s — a single non-blocking
        // drain returned NoCard immediately and pcscd's connect (which
        // power-offs at open) always failed. Bounded wait: 8 x 150 ms
        // via Transport::wait_ms (no-op in host mocks).
        // The budget is PER-CALL: without this reset it was a one-time
        // consumable and every power_on after the first ~2 connects did
        // zero waiting — the "present but unresponsive" bench state.
        self.collect_rounds = 0;
        let (fresh_ntf, fresh_ats) = loop {
            match reader::wait_for_event(&mut self.transport) {
                Some(reader::TagEvent::Tag(n, ats)) => {
                    self.last_ntf = Some(n);
                    self.last_auto_ats = ats.clone();
                    break (Some(n), ats);
                }
                // The tag left the field since the last presence poll:
                // the cached NTF/ATS is stale and must NOT be
                // resurrected into a phantom ATR (issue #109 item 2).
                Some(reader::TagEvent::LinkLost) => {
                    self.clear_tag();
                    return Err(Error::NoCard);
                }
                None => {}
            }
            if self.collect_rounds >= 8 {
                break (None, None);
            }
            self.collect_rounds += 1;
            self.transport.wait_ms(150);
        };
        // Phantom-latch guard (bench 2026-10-13): discovery is running
        // (the preceding power_off restarted it) — a present tag
        // re-activates within the collect window. Nothing fresh AND no
        // cached AUTO-activation means the tag left while its plain
        // discovery NTF stayed latched: selecting against that stale
        // id fails forever and traps pcscd in a connect-retry loop
        // that churns the chip into send-mute. Clear and fail clean;
        // a real re-arrival raises a fresh NTF.
        if fresh_ntf.is_none() && self.last_auto_ats.is_none() && self.last_ntf.is_some() {
            self.clear_tag();
            return Err(Error::NoCard);
        }
        // Wall-power re-activation (bench 2026-10-13): the card coupled
        // during boot, the activation NTF was consumed by the first
        // presence poll, and a later exchange failure cleared the cache.
        // The NFCC still holds the RF interface open (auto-activated)
        // but won't re-fire the NTF. Force a fresh activation cycle:
        // deactivate to discovery, then collect again — the present
        // tag re-activates within ~0.5 s.
        if fresh_ntf.is_none() && self.last_ntf.is_none() {
            let _ = reader::deactivate_to_discovery(&mut self.transport);
            self.collect_rounds = 0;
            let (ntf2, ats2) = loop {
                match reader::wait_for_event(&mut self.transport) {
                    Some(reader::TagEvent::Tag(n, ats)) => {
                        self.last_ntf = Some(n);
                        self.last_auto_ats = ats.clone();
                        break (Some(n), ats);
                    }
                    Some(reader::TagEvent::LinkLost) => {
                        self.clear_tag();
                        return Err(Error::NoCard);
                    }
                    None => {}
                }
                if self.collect_rounds >= 8 {
                    break (None, None);
                }
                self.collect_rounds += 1;
                self.transport.wait_ms(150);
            };
            if ntf2.is_none() {
                return Err(Error::NoCard);
            }
            let ntf = ntf2.unwrap();
            let ats: heapless::Vec<u8, 32> = match ats2 {
                Some(a) => a,
                None => return Err(Error::Select("re-activation carried no ATS")),
            };
            if let Some(uid) = reader::nfca_uid(&ntf) {
                self.uid_len = uid.len().min(10);
                self.uid[..self.uid_len].copy_from_slice(&uid[..self.uid_len]);
            }
            let atr_bytes = crate::transport::ats_to_atr(&ats)
                .ok_or(Error::Select("ATS too short for ATR construction"))?;
            if atr.len() < atr_bytes.len() {
                return Err(Error::BufferTooSmall);
            }
            atr[..atr_bytes.len()].copy_from_slice(&atr_bytes);
            self.active = true;
            return Ok(atr_bytes.len());
        }
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

    /// Deactivate the RF interface and return the reader to POLLING.
    pub fn power_off(&mut self) {
        // CCID power-off semantics: the card powers down but the reader
        // keeps polling — that is how the next insertion is seen. Going
        // to IDLE instead left no RF_DISCOVER running, so the
        // auto-activation NTF never re-fired and presence (GSS bStatus)
        // read absent forever after a host IccPowerOff (bench
        // 2026-10-11: pcscd "Card removed" after its open-sequence
        // power-off; bStatus 01 → 02 stuck while the card sat on the
        // coil).
        // Lazy teardown (bench 2026-10-13): pcscd's connect sequence
        // ALWAYS power-offs immediately before power-on — deactivating
        // the auto-activated tag destroys the only good activation a
        // marginal card gives per field cycle, and its re-activation
        // after teardown is not reliable. Keep the RF interface: mark
        // the session logically off and let the next power_on rebuild
        // from the cached auto-ATS (no select, no re-activation). The
        // interface frees on link-lost, or on a power_off with no
        // cached activation (the eager path below).
        if self.last_auto_ats.is_some() {
            self.active = false;
            self.probe_armed = true;
            self.discovery_parked = false;
            return;
        }
        // Eager path: only a SELECTED session holds an RF interface
        // without a cached auto-ATS — tear that one down (single
        // deactivate; the re-issue is refused 0xA0 when discovery is
        // already running). A plain discovery-NTF latch has no
        // interface: no chip writes at all.
        if self.active && reader::deactivate_to_discovery(&mut self.transport).is_err() {
            let _ = reader::restart_discovery(&mut self.transport);
            // Same multi-frame residue risk as the resync path (Codex
            // PR159 P1): empty the link after the fallback chain.
            reader::drain_link(&mut self.transport);
        }
        self.active = false;
        self.uid_len = 0;
        self.last_ntf = None;
        self.last_auto_ats = None;
        self.rearm = None;
        self.probe_armed = true;
        self.discovery_parked = false;
        self.silent_rearms = 0;
    }

    /// Exchange one APDU with the activated tag (connection 0).
    ///
    /// Issue #156 A (the #151 desync hole): on a collection TIMEOUT the
    /// RF interface is still live and the chip keeps honoring WTX — the
    /// late DATA response lands in the NEXT command's poll window
    /// (#101-class desync). Timeout therefore tears down and resyncs:
    /// deactivate-to-discovery (fallback: restart discovery), drain the
    /// link, clear the tag latch, close the session — the mirror of
    /// `power_off`'s eager teardown. LINK LOSS needs none of that: the
    /// chip already dropped the interface itself.
    pub fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Error> {
        if !self.active {
            return Err(Error::NoCard);
        }
        let (outcome, wtx) =
            reader::exchange_detailed(&mut self.transport, 0, command, self.stall_hook);
        self.wtx_last = wtx;
        self.wtx_total = self.wtx_total.saturating_add(wtx);
        match outcome {
            ExchangeOutcome::Data(rsp) => {
                // Stale-ACTIVE guard (bench 2026-10-13): every real APDU response
                // carries SW (>= 2 bytes) — an EMPTY exchange means the tag faded
                // without a link-lost NTF and the lazy-teardown cache is holding a
                // phantom session. Clear the latch so presence reads absent and
                // discovery re-arms for the next real coupling.
                if rsp.is_empty() {
                    self.clear_tag();
                    self.active = false;
                    self.uid_len = 0;
                    return Err(Error::ExchangeFailed);
                }
                if response.len() < rsp.len() {
                    return Err(Error::BufferTooSmall);
                }
                response[..rsp.len()].copy_from_slice(&rsp);
                Ok(rsp.len())
            }
            ExchangeOutcome::Timeout => {
                // Resync teardown (issue #156 A): the chip may still be
                // mid-WTX on a live interface — tear it down and empty
                // the link so no late DATA response pollutes the next
                // command window. drain-until-EMPTY with a settle beat
                // (issue #170): WTX-honoring chips deliver the late
                // response AFTER the burst — the settle round catches
                // it (Codex PR159 P1 bounded the burst; this bounds the
                // just-missed tail).
                if reader::deactivate_to_discovery(&mut self.transport).is_err() {
                    let _ = reader::restart_discovery(&mut self.transport);
                }
                reader::settle_drain(&mut self.transport);
                self.clear_tag();
                self.active = false;
                self.uid_len = 0;
                Err(Error::ExchangeFailed)
            }
            ExchangeOutcome::LinkLost => {
                self.teardown_on_link_loss();
                Err(Error::ExchangeFailed)
            }
        }
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
    use crate::Frame;
    use crate::{
        DEACTIVATE_REASON_DH_REQ, DEACTIVATE_REASON_RF_LINK_LOSS_NCI1X,
        DEACTIVATE_REASON_RF_LINK_LOSS_NXP, DEACTIVATE_TYPE_IDLE, GID_CORE, GID_RF, MT_CMD, MT_NTF,
        MT_RSP, NCI_INTERFACE_ISO_DEP, NCI_PROTOCOL_ISO_DEP, NTF_RF_DEACTIVATE, NTF_RF_DISCOVER,
        NTF_RF_INTF_ACTIVATED, OID_CORE_INIT, OID_CORE_RESET, OID_RF_DEACTIVATE, OID_RF_DISCOVER,
        OID_RF_DISCOVER_MAP, OID_RF_DISCOVER_SELECT, STATUS_OK,
    };

    // A realistic ISO-DEP ATS (per ISO 14443-4: TL, T0, TA1, TB1)
    // Proper ATS: TL=5, T0=0x75, TA1=0x77, TB1=0x81, TC1=0x02
    const TEST_ATS: [u8; 6] = [0x75, 0x77, 0x81, 0x02, 0x80, 0x73];
    // PC/SC ATR constructed from the ATS: 0x3B replaces TL, body follows
    const TEST_ATR: [u8; 9] = [0x3B, 0x82, 0x81, 0x31, 0xFE, 0x45, 0x80, 0x73, 0x7A];

    fn script_ladder(t: &mut MockTransport) {
        t.push_reply(&[MT_RSP | GID_CORE, OID_CORE_RESET, 0x01, STATUS_OK]);
        t.push_notification(&[MT_NTF, OID_CORE_RESET, 0x01, 0x00]);
        t.push_reply(&[MT_RSP | GID_CORE, OID_CORE_INIT, 0x01, STATUS_OK]);
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
            0x0C,
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
            TEST_ATS[5],
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

    #[test]
    fn timeout_resync_tears_down_interface_drains_and_clears() {
        // Issue #156 A: on a collection timeout the RF interface may
        // still be live (chip honoring WTX) — the driver must tear it
        // down, drain the link, clear the latch and close the session
        // so the late DATA response cannot land in the next command's
        // poll window (#101-class desync).
        let mut t = MockTransport::new();
        script_discover_select_activate(&mut t);
        // no DATA reply, no deactivate RSPs scripted: the teardown hits
        // its fallback chain (deactivate fails → restart_discovery).

        let mut drv = Pn7160Driver::new(t);
        let mut atr = [0u8; 32];
        drv.power_on(&mut atr).expect("power_on");
        assert!(drv.session_active());

        let mut rsp = [0u8; 256];
        assert_eq!(
            drv.transmit_apdu(&[0x00], &mut rsp),
            Err(Error::ExchangeFailed)
        );
        assert!(!drv.session_active(), "timeout closes the session");
        let sent = &drv.transport_mut().sent;
        let teardown_written = sent
            .iter()
            .any(|c| c.as_slice() == reader::RF_DEACTIVATE_DISCOVERY);
        assert!(
            teardown_written,
            "resync must attempt deactivate-to-discovery: {:?}",
            sent
        );
        // The next command cannot ride the dead session.
        assert_eq!(drv.transmit_apdu(&[0x00], &mut rsp), Err(Error::NoCard));
    }

    // given a timeout exchange on a link carrying MULTI-frame pending
    // traffic (Codex PR159 P1): a late DATA packet + WTX advisory
    // ahead of the teardown RSP (the masquerade), the deactivate NTF,
    // then MORE late residue after a quiet beat — when transmit_apdu
    // resyncs, then the teardown consumes the churn via strict RSP
    // matching, drains until the link is ACTUALLY empty (a survivor
    // would land in the next command's poll window), and closes the
    // session.
    struct ResyncTransport {
        sent: std::vec::Vec<std::vec::Vec<u8>>,
        polls: usize,
        poll_schedule: std::vec::Vec<Option<std::vec::Vec<u8>>>,
        drain_pos: usize,
        drain_schedule: std::vec::Vec<Option<std::vec::Vec<u8>>>,
    }
    impl Transport for ResyncTransport {
        fn send(&mut self, cmd: &[u8]) -> bool {
            self.sent.push(cmd.to_vec());
            true
        }
        fn poll_frame(&mut self) -> Option<Frame> {
            let raw = self.poll_schedule.get(self.polls)?.clone();
            self.polls += 1;
            raw.and_then(|r| Frame::decode(&r))
        }
        fn transact(&mut self, _cmd: &[u8]) -> Option<Frame> {
            None
        }
        fn drain(&mut self) -> Option<Frame> {
            let raw = self.drain_schedule.get(self.drain_pos)?.clone();
            self.drain_pos += 1;
            raw.and_then(|r| Frame::decode(&r))
        }
    }

    #[test]
    fn timeout_resync_drains_multi_frame_late_traffic_to_empty() {
        let raw = |b: &[u8]| Some(b.to_vec());
        let mut poll_schedule: std::vec::Vec<Option<std::vec::Vec<u8>>> =
            (0..12).map(|_| None).collect(); // exchange RX: 12 silent rounds → Timeout
        poll_schedule.push(raw(&[0x00, 0x00, 0x02, 0x90, 0x00])); // late DATA packet (masquerade bait)
        poll_schedule.push(raw(&[0x6F, 0x17, 0x00])); // WTX advisory
        poll_schedule.push(raw(&[0x41, 0x06, 0x01, 0x00])); // RF_DEACTIVATE_RSP OK
        let mut drain_schedule: std::vec::Vec<Option<std::vec::Vec<u8>>> = std::vec::Vec::new();
        drain_schedule.extend([raw(&auto_activation_ntf())]); // presence latch
        drain_schedule.extend((0..9).map(|_| None)); // power_on collect rounds
        drain_schedule.push(raw(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x02,
            DEACTIVATE_TYPE_IDLE,
            DEACTIVATE_REASON_DH_REQ,
        ])); // the teardown's own NTF (eaten by deactivate's drain)
        drain_schedule.push(None); // link quiet — first drain_link stops here
        drain_schedule.push(raw(&[0x10, 0x00, 0x01, 0xAA])); // late DATA segment arrives
        drain_schedule.push(raw(&[0x41, 0x03, 0x01, 0x00])); // stale RF_DISCOVER_RSP
        drain_schedule.push(raw(&[0x6F, 0x17, 0x00])); // WTX advisory
        drain_schedule.push(None); // actually empty now

        let mut drv = Pn7160Driver::new(ResyncTransport {
            sent: std::vec::Vec::new(),
            polls: 0,
            poll_schedule,
            drain_pos: 0,
            drain_schedule,
        });
        assert!(drv.is_card_present(), "presence latches the activation NTF");
        let mut atr = [0u8; 32];
        drv.power_on(&mut atr)
            .expect("power_on via cached auto-ATS");
        assert!(drv.session_active());

        let mut rsp = [0u8; 256];
        assert_eq!(
            drv.transmit_apdu(&[0x00, 0xA4, 0x04, 0x00], &mut rsp),
            Err(Error::ExchangeFailed),
            "the silent 2.4 s collect window times out"
        );
        assert!(!drv.session_active(), "resync closes the session");

        let t = drv.transport_mut();
        assert_eq!(
            t.polls,
            t.poll_schedule.len(),
            "every scripted frame consumed — nothing rides the poll window"
        );
        assert!(
            t.sent
                .iter()
                .any(|c| c.as_slice() == reader::RF_DEACTIVATE_DISCOVERY),
            "strict teardown wrote the deactivate"
        );
        assert!(
            !t.sent
                .iter()
                .any(|c| c.len() >= 2 && c[0] == MT_CMD | GID_RF && c[1] == OID_RF_DISCOVER),
            "matching RSP arrived — no fallback discover needed"
        );
        // Multi-frame residue drained to EMPTY: both queues exhausted.
        assert!(t.poll_frame().is_none());
        assert!(t.drain().is_none());
        // And the dead session rejects the next command.
        assert_eq!(drv.transmit_apdu(&[0x00], &mut rsp), Err(Error::NoCard));
    }

    #[test]
    fn link_lost_mid_exchange_clears_session_without_teardown_writes() {
        // Issue #156 A2: a link-lost NTF mid-exchange means the chip
        // already tore the RF interface down — no deactivate/restart
        // writes, just the session clear.
        let mut t = MockTransport::new();
        script_discover_select_activate(&mut t);
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x02,
            0x00,
            DEACTIVATE_REASON_RF_LINK_LOSS_NCI1X,
        ]);

        let mut drv = Pn7160Driver::new(t);
        let mut atr = [0u8; 32];
        drv.power_on(&mut atr).expect("power_on");

        let mut rsp = [0u8; 256];
        assert_eq!(
            drv.transmit_apdu(&[0x00], &mut rsp),
            Err(Error::ExchangeFailed)
        );
        assert!(!drv.session_active());
        let sent = &drv.transport_mut().sent;
        assert!(
            !sent
                .iter()
                .any(|c| c.as_slice() == reader::RF_DEACTIVATE_DISCOVERY),
            "link-loss teardown writes nothing: {:?}",
            sent
        );
    }

    #[test]
    fn wtx_counters_track_advisories_per_exchange() {
        // Issue #156 A3: last-exchange and lifetime WTX advisory counts
        // (Escape 0xD4 surface) across two exchanges in one session.
        let mut t = MockTransport::new();
        script_discover_select_activate(&mut t);
        t.push_notification(&[0x6F, 0x17, 0x00]);
        t.push_notification(&[0x6F, 0x17, 0x00]);
        t.push_notification(&[0x00, 0x00, 0x02, 0x90, 0x00]);
        t.push_notification(&[0x6F, 0x17, 0x00]);
        t.push_notification(&[0x00, 0x00, 0x02, 0x61, 0x02]);

        let mut drv = Pn7160Driver::new(t);
        let mut atr = [0u8; 32];
        drv.power_on(&mut atr).expect("power_on");
        assert_eq!(drv.wtx_counters(), (0, 0), "counters start zeroed");

        let mut rsp = [0u8; 256];
        drv.transmit_apdu(&[0x00, 0xA4, 0x04, 0x00], &mut rsp)
            .expect("apdu");
        assert_eq!(drv.wtx_counters(), (2, 2), "last + total after exchange 1");

        drv.transmit_apdu(&[0x00, 0xA4, 0x04, 0x00], &mut rsp)
            .expect("apdu 2");
        assert_eq!(
            drv.wtx_counters(),
            (1, 3),
            "last resets per exchange, total accumulates"
        );
    }

    #[test]
    fn transmit_fragmented_apdu_reassembles_response() {
        // GP STORE DATA shape through the driver layer: a 260-octet
        // command fragments 253 + 7 at the NCI layer (PBF chain, credit
        // gate between fragments — lib.rs exchange tests); the response
        // passes transmit_apdu's existing bounds check untouched.
        let mut t = MockTransport::new();
        script_discover_select_activate(&mut t);
        t.push_reply(&[0x60, 0x06, 0x03, 0x01, 0x00, 0x01]); // credit re-grant
        script_apdu(&mut t, &[0x90, 0x00]);

        let mut drv = Pn7160Driver::new(t);
        let mut atr = [0u8; 32];
        drv.power_on(&mut atr).expect("power_on");

        let mut rsp = [0u8; 1024];
        let n = drv
            .transmit_apdu(&[0xA5u8; 260], &mut rsp)
            .expect("fragmented apdu");
        assert_eq!(&rsp[..n], &[0x90, 0x00]);

        let t = drv.transport_mut();
        // sent = [RF_DISCOVER_SELECT (power_on), DATA fragment 1, DATA
        // fragment 2].
        assert_eq!(t.sent.len(), 3);
        assert_eq!(t.sent[1].len(), 3 + 253);
        assert_eq!(t.sent[1][0], 0x10, "first fragment PBF set");
        assert_eq!(t.sent[2].len(), 3 + 7);
        assert_eq!(t.sent[2][0], 0x00, "last fragment PBF clear");
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

    // given a powered-on session (bench 2026-10-11: pcscd's open
    // sequence always sends IccPowerOff), when power_off runs, then the
    // reader returns to DISCOVERY (RF_DEACTIVATE type=DISCOVERY, not
    // IDLE) and the next auto-activation NTF re-establishes presence —
    // from IDLE no discovery was running and presence read absent
    // forever ("Card removed" in pcscd while the card sat on the coil).
    #[test]
    fn power_off_returns_to_discovery_and_presence_re_latches() {
        let mut t = MockTransport::new();
        t.push_notification(&auto_activation_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());
        let mut atr = [0u8; 32];
        let n = drv.power_on(&mut atr).expect("power_on");
        assert!(n > 0);

        // Lazy teardown (bench 2026-10-13): pcscd's connect sequence
        // power-offs immediately before power-on — tearing down the
        // auto-activated tag destroys the only good activation a
        // marginal card gives per field cycle. power_off keeps the RF
        // interface: zero chip writes, session logically off.
        drv.power_off();
        assert!(!drv.session_active());
        assert_eq!(
            drv.transport_mut().sent.len(),
            0,
            "lazy teardown writes nothing"
        );
        assert!(
            drv.is_card_present(),
            "the auto-activated tag stays present across the logical off"
        );

        // The payoff: pcscd's immediate power_on rebuilds the ATR from
        // the cached auto-ATS — no select, no re-activation needed.
        let mut atr2 = [0u8; 32];
        let n2 = drv.power_on(&mut atr2).expect("power_on after lazy off");
        assert!(n2 > 0);

        // the field re-polls: the next auto-activation re-latches
        drv.transport_mut()
            .push_notification(&auto_activation_ntf());
        assert!(drv.is_card_present());
    }

    // The #125 bench repro as a host test: GSS present → host
    // IccPowerOff (pcscd's open sequence always sends it first) →
    // presence must recover via the RF_DISCOVER re-issue and power_on
    // must return a fresh ATR — the chip does NOT restart polling on
    // its own after a session teardown.
    #[test]
    fn poweroff_stuck_chip_recovers_presence_and_atr() {
        let mut t = MockTransport::new();
        t.push_notification(&auto_activation_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());
        let mut atr = [0u8; 32];
        assert!(drv.power_on(&mut atr).is_ok());

        // power_off: deactivate RSP OK; the chip answers the discover
        // re-issue OK. The activation NTF is pushed AFTER — the chip
        // auto-activates ~TOTAL_DURATION after the discover write (and
        // power_off's deactivate drain would eat a pre-queued NTF).
        drv.transport_mut()
            .push_reply(&[MT_RSP | GID_RF, OID_RF_DEACTIVATE, 0x01, STATUS_OK]);
        drv.transport_mut()
            .push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER, 0x01, STATUS_OK]);
        drv.power_off();
        drv.transport_mut()
            .push_notification(&auto_activation_ntf());

        // Recovery window: the activation NTF lands within the next
        // polls (bench: ~TOTAL_DURATION after the discover).
        let mut present = false;
        for _ in 0..REARM_NTF_POLLS + 1 {
            if drv.is_card_present() {
                present = true;
                break;
            }
        }
        assert!(present, "presence re-latches after the discover re-issue");

        let mut atr2 = [0u8; 32];
        let n = drv.power_on(&mut atr2).expect("power_on after power_off");
        assert!(n > 0);
        assert!(!drv
            .into_transport()
            .sent
            .iter()
            .any(|f| f.len() >= 2 && f[0] == MT_CMD | GID_RF && f[1] == OID_RF_DISCOVER_SELECT));
    }

    // given an empty re-arm window with the probe armed (a presence edge
    // happened), when the window expires, then exactly ONE RF_DISCOVER
    // probe is written — never per-cycle (chip mute after ~11, bench
    // 2026-10-10) — and a probe RSP failure is survivable.
    #[test]
    fn rearm_expiry_probes_discovery_once_and_survives_rejection() {
        for probe_rsp_ok in [true, false] {
            let mut t = MockTransport::new();
            t.push_notification(&auto_activation_ntf());
            let mut drv = Pn7160Driver::new(t);
            assert!(drv.is_card_present(), "latch arms the probe");

            // Absence: the link-loss NTF clears the latch (re-arming).
            drv.transport_mut().push_notification(&[
                MT_NTF | GID_RF,
                OID_RF_DEACTIVATE,
                0x02,
                DEACTIVATE_TYPE_IDLE,
                DEACTIVATE_REASON_RF_LINK_LOSS_NXP,
            ]);
            assert!(!drv.is_card_present());

            // Run the deactivate re-arm to its empty NTF window: the
            // deactivate RSP comes back OK, no tag NTF follows. The
            // probe RSP is OK or a status rejection, per the loop.
            let t = drv.transport_mut();
            t.push_reply(&[MT_RSP | GID_RF, OID_RF_DEACTIVATE, 0x01, STATUS_OK]);
            t.push_reply(&[
                MT_RSP | GID_RF,
                OID_RF_DISCOVER,
                0x01,
                if probe_rsp_ok { STATUS_OK } else { 0x05 },
            ]);

            for _ in 0..(PRESENCE_REARM_POLLS as usize
                + REARM_RSP_POLLS as usize
                + REARM_NTF_POLLS as usize
                + 4)
            {
                drv.is_card_present();
            }
            let t = drv.transport_mut();
            let probe_writes = t
                .sent
                .iter()
                .filter(|f| f.len() >= 2 && f[0] == MT_CMD | GID_RF && f[1] == OID_RF_DISCOVER)
                .count();
            assert_eq!(
                probe_writes, 1,
                "exactly one discover probe after the edge (rsp_ok={probe_rsp_ok})"
            );

            // Rejection path: no phase stuck; the steady cadence may
            // write its next DEACTIVATE (expected), but never another
            // DISCOVER probe.
            for _ in 0..(PRESENCE_REARM_POLLS as usize + 4) {
                drv.is_card_present();
            }
            let discovers_after = drv
                .transport_mut()
                .sent
                .iter()
                .filter(|f| f.len() >= 2 && f[0] == MT_CMD | GID_RF && f[1] == OID_RF_DISCOVER)
                .count();
            assert_eq!(
                discovers_after, probe_writes,
                "probe not repeated after its window (rsp_ok={probe_rsp_ok})"
            );
        }
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
            &[0x3B, 0x85, 0x81, 0x31, 0xFE, 0x45, 0x80, 0x73, 0xC8, 0x21, 0x10, 0x84],
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
    // with reason RF-link-lost (the tag left the field — NCI §5.2.5.2
    // Table 81), then presence clears immediately; a DH-requested
    // deactivation (our own teardown, reason 0x00) must not be mistaken
    // for removal. Both disputed link-loss candidates are covered (see
    // the DEACTIVATE_REASON_* block comment in lib.rs).
    #[test]
    fn rf_link_lost_clears_auto_presence() {
        for reason in [
            DEACTIVATE_REASON_RF_LINK_LOSS_NCI1X,
            DEACTIVATE_REASON_RF_LINK_LOSS_NXP,
        ] {
            let mut t = MockTransport::new();
            t.push_notification(&auto_activation_ntf());
            let mut drv = Pn7160Driver::new(t);
            assert!(
                drv.is_card_present(),
                "reason {:#x}: latch activation",
                reason
            );

            // reason 0x00 = DH request (e.g. a host power_off in
            // flight) — not a removal; presence unchanged.
            drv.transport_mut().push_notification(&[
                MT_NTF | GID_RF,
                NTF_RF_DEACTIVATE,
                0x02,
                DEACTIVATE_TYPE_IDLE,
                DEACTIVATE_REASON_DH_REQ,
            ]);
            assert!(
                drv.is_card_present(),
                "reason {:#x}: DH-requested deactivate ≠ removal",
                reason
            );

            // link-loss — the tag is gone.
            drv.transport_mut().push_notification(&[
                MT_NTF | GID_RF,
                NTF_RF_DEACTIVATE,
                0x02,
                DEACTIVATE_TYPE_IDLE,
                reason,
            ]);
            assert!(
                !drv.is_card_present(),
                "reason {:#x}: link loss = immediate absent",
                reason
            );
        }
    }

    // --- Issue #109 item 2: active-session removal + stale reuse ---

    // given a LIVE session (power_on done, APDUs answered), when the tag
    // is removed mid-session (link-loss NTF), then the next presence poll
    // tears the session down — presence false, session_active false,
    // transmit rejected — and the teardown writes nothing to the wire
    // (the chip already dropped the RF interface itself).
    #[test]
    fn active_session_link_loss_tears_down_session() {
        let mut t = MockTransport::new();
        script_discover_select_activate(&mut t);
        script_apdu(&mut t, &[0x90, 0x00]);
        let mut drv = Pn7160Driver::new(t);
        let mut atr = [0u8; 32];
        drv.power_on(&mut atr).expect("power_on");
        let mut rsp = [0u8; 256];
        drv.transmit_apdu(&[0x00], &mut rsp).expect("apdu");

        drv.transport_mut().push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x02,
            DEACTIVATE_TYPE_IDLE,
            DEACTIVATE_REASON_RF_LINK_LOSS_NXP,
        ]);
        let sent_before = drv.transport_mut().sent.len();
        assert!(!drv.is_card_present(), "mid-session removal flips presence");
        assert!(!drv.session_active(), "session state cleared");
        assert_eq!(
            drv.transport_mut().sent.len(),
            sent_before,
            "teardown must not write — the chip dropped the interface"
        );
        assert_eq!(
            drv.transmit_apdu(&[0x00], &mut rsp),
            Err(Error::NoCard),
            "dead session rejects APDUs"
        );
    }

    // given a live session, when a DH-requested deactivate NTF arrives
    // (echo of our own deactivate paths), then the session stays up.
    #[test]
    fn dh_request_deactivate_during_session_is_not_removal() {
        let mut t = MockTransport::new();
        script_discover_select_activate(&mut t);
        let mut drv = Pn7160Driver::new(t);
        let mut atr = [0u8; 32];
        drv.power_on(&mut atr).expect("power_on");

        drv.transport_mut().push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x02,
            DEACTIVATE_TYPE_IDLE,
            DEACTIVATE_REASON_DH_REQ,
        ]);
        assert!(drv.is_card_present());
        assert!(drv.session_active(), "DH-request ≠ mid-session removal");
    }

    // given the tag was removed between presence polls (link-loss NTF
    // queued while the cache still holds the activation), when the host
    // calls power_on directly, then the drain consumes the link-loss,
    // discards the stale cache, and answers NoCard — no phantom ATR from
    // the stale ATS, no select from the stale discovery (issue #109
    // item 2: "power_on may also reuse a stale activation NTF").
    #[test]
    fn power_on_consumes_link_loss_and_rejects_stale_ntf() {
        let mut t = MockTransport::new();
        t.push_notification(&auto_activation_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present(), "latch the auto-activation");

        drv.transport_mut().push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x02,
            DEACTIVATE_TYPE_IDLE,
            DEACTIVATE_REASON_RF_LINK_LOSS_NCI1X,
        ]);
        let mut atr = [0u8; 32];
        assert_eq!(
            drv.power_on(&mut atr),
            Err(Error::NoCard),
            "stale activation must not be resurrected"
        );
        assert!(!drv.is_card_present(), "cache cleared, stays absent");
        let recovered = drv.into_transport();
        assert!(
            !recovered
                .sent
                .iter()
                .any(|f| f.len() >= 2 && f[0] == MT_CMD | GID_RF && f[1] == OID_RF_DISCOVER_SELECT),
            "no select may be attempted from a stale cache"
        );
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
    /// A cycle may add the one-shot #125 RF_DISCOVER probe when armed —
    /// so the cycle budget is the deactivate+probe pair, still one
    /// write per poll.
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
                wrote <= writes_before + 2,
                "poll {}: cycle wrote more than the deactivate+probe pair",
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

    /// #326: "parked forever" is a deadlock on host-driven poll paths
    /// (vpcd: the host is the only poller). After the two empty re-arms
    /// park the driver, continued polling must STILL re-arm at the slow
    /// parked cadence — no fast cycling (the ~11-rapid-cycle chip-mute
    /// class), but never a permanent deaf state — and a tag NTF during
    /// the slow cycle must restore presence.
    /// #328/#332 (Codex P1): the parked re-arm fires by TIME when the
    /// host stops polling entirely (pcscd marks the reader dead) — no
    /// is_card_present call needed, only serve-loop maintenance ticks.
    #[test]
    fn parked_rearm_fires_by_time_without_host_polls() {
        let mut t = MockTransport::new();
        t.push_notification(&discover_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());
        run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
        run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
        assert!(drv.discovery_parked);

        let writes0 = drv.transport_mut().sent.len();
        // NO host polls — only maintenance ticks with an injected clock.
        // The FIRST tick is immediately due (last_rearm_ms unknown — a
        // parked driver gets one prompt time-re-arm, then the cadence).
        drv.transport_mut()
            .push_reply(&deactivate_discovery_rsp_ok());
        drv.poll_maintenance(1_000);
        assert_eq!(
            drv.transport_mut().sent[writes0].as_slice(),
            reader::RF_DEACTIVATE_DISCOVERY.as_slice(),
            "first maintenance tick fires the parked re-arm (unknown last time = due)"
        );
        // the phase machine completes on later ticks: deactivate RSP ->
        // AwaitNtf expiry -> the latched #125 probe (RSP scripted too)
        // -> None. Ticks are generous.
        drv.transport_mut()
            .push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER, 0x01, STATUS_OK]);
        for _ in 0..16 {
            drv.poll_maintenance(20_100);
        }
        assert_eq!(
            drv.rearm, None,
            "maintenance advances the machine through the NTF window and any latched probe"
        );
        // cadence: within 12.8 s of the last re-arm write, ticks stay
        // silent; the next write comes only after the window.
        let quiescent = drv.transport_mut().sent.len();
        for _ in 0..8 {
            drv.poll_maintenance(25_000);
        }
        assert_eq!(
            drv.transport_mut().sent.len(),
            quiescent,
            "no re-arm write inside the 12.8 s window"
        );
        drv.transport_mut()
            .push_reply(&deactivate_discovery_rsp_ok());
        drv.poll_maintenance(35_000);
        assert!(
            drv.transport_mut().sent.len() > quiescent,
            "re-arm fires again after the full window"
        );
    }

    /// #349 (Codex P1): a REFUSED deactivate (NFCC truly idle) must
    /// escalate to RF_DISCOVER — not retry the refused command forever.
    #[test]
    fn refused_deactivate_rearm_escalates_to_rf_discover() {
        let mut t = MockTransport::new();
        t.push_notification(&discover_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());

        run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
        run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
        assert!(drv.discovery_parked);

        // settle the #125 one-shot probe if armed
        drv.transport_mut()
            .push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER, 0x01, STATUS_OK]);
        for _ in 0..16 {
            let _ = drv.is_card_present();
        }
        // drain any stale queued frames so the scripted sequence below
        // is consumed by the phases it was written for
        for _ in 0..64 {
            let _ = drv.is_card_present();
        }
        let settled = drv.transport_mut().sent.len();

        // the parked re-arm: deactivate REFUSED (status != OK)
        let refused = [MT_RSP | GID_RF, OID_RF_DEACTIVATE, 0x01, 0xA0];
        drv.transport_mut().push_reply(&refused);
        // then the escalated RF_DISCOVER's RSP + a recovery NTF
        drv.transport_mut()
            .push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER, 0x01, STATUS_OK]);
        drv.transport_mut().push_notification(&discover_ntf());

        let mut escalated = false;
        for _ in 0..(PARKED_REARM_POLLS as usize + 4 * REARM_RSP_POLLS as usize + 32) {
            let _ = drv.is_card_present();
            let sent = &drv.transport_mut().sent;
            if sent.len() >= settled + 2 {
                assert_eq!(
                    sent[settled].as_slice(),
                    reader::RF_DEACTIVATE_DISCOVERY.as_slice()
                );
                assert_eq!(
                    sent[settled + 1].as_slice(),
                    reader::RF_DISCOVER_PASSIVE_A.as_slice(),
                    "refused deactivate must escalate to RF_DISCOVER"
                );
                escalated = true;
                break;
            }
        }
        assert!(escalated, "escalation never happened");
        // and the NTF restores presence
        for _ in 0..8 {
            if drv.is_card_present() {
                break;
            }
        }
        assert!(drv.is_card_present(), "recovery NTF must restore presence");
    }

    /// #349 (Codex P1): a silent escalated probe counts as a silent
    /// re-arm so the wifi VEN-cycle threshold can fire.
    #[test]
    fn silent_discover_probe_counts_as_silent_rearm() {
        let mut t = MockTransport::new();
        t.push_notification(&discover_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());
        run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
        run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
        assert!(drv.discovery_parked);

        // settle the #125 one-shot probe if armed
        drv.transport_mut()
            .push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER, 0x01, STATUS_OK]);
        for _ in 0..16 {
            let _ = drv.is_card_present();
        }

        // parked re-arm with NO responses at all: deactivate written,
        // its RSP never arrives -> give-up (silent +1)
        let before = drv.silent_rearms;
        for _ in 0..(PARKED_REARM_POLLS as usize + REARM_RSP_POLLS as usize + 8) {
            let _ = drv.is_card_present();
        }
        assert!(drv.silent_rearms > before, "silent AwaitRsp must count");
        assert_eq!(drv.rearm, None);

        // now a re-arm whose deactivate is REFUSED and whose RF_DISCOVER
        // RSP never arrives: the probe give-up must ALSO count.
        let refused = [MT_RSP | GID_RF, OID_RF_DEACTIVATE, 0x01, 0xA0];
        drv.transport_mut().push_reply(&refused);
        let before2 = drv.silent_rearms;
        for _ in 0..(PARKED_REARM_POLLS as usize + 2 * REARM_RSP_POLLS as usize + 8) {
            let _ = drv.is_card_present();
        }
        assert!(
            drv.silent_rearms > before2,
            "silent escalated probe must count toward the VEN-cycle threshold"
        );
    }

    #[test]
    fn parked_driver_still_rearms_at_slow_cadence_and_recovers() {
        let mut t = MockTransport::new();
        t.push_notification(&discover_ntf());
        let mut drv = Pn7160Driver::new(t);
        assert!(drv.is_card_present());

        // Two empty re-arm cycles → cleared + parked.
        run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
        run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
        assert!(!drv.is_card_present());
        assert!(drv.discovery_parked, "expected parked after 2 empty cycles");

        // The #125 one-shot RF_DISCOVER probe may fire right after the
        // park (probe_armed was set by the earlier latch) — let it
        // complete before measuring the parked cadence.
        drv.transport_mut()
            .push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER, 0x01, STATUS_OK]);
        for _ in 0..16 {
            let _ = drv.is_card_present();
        }

        // Parked: polling must (a) STILL re-arm — a new deactivate write
        // within one slow-cadence budget (the #326 deadlock fix) — and
        // (b) NOT fast-cycle: no window of 8 consecutive polls writes
        // more than the deactivate+probe pair (the ~11-rapid-cycle
        // chip-mute class stays out of reach).
        let writes_before = drv.transport_mut().sent.len();

        // settle: let the one-shot #125 probe (if pending) complete
        for _ in 0..16 {
            let _ = drv.is_card_present();
        }
        let settled = drv.transport_mut().sent.len();
        assert!(
            settled <= writes_before + 2,
            "probe settle wrote more than the pair: {settled} vs {writes_before}"
        );

        // the parked re-arm itself: a new write within the slow cadence.
        // #349: step 1 is ALWAYS the #105 deactivate (perpetual-
        // discovery parked state — the NORMAL case); its OK RSP is
        // seeded here.
        drv.transport_mut()
            .push_reply(&deactivate_discovery_rsp_ok());
        let mut rearmed = false;
        for i in 0..(PARKED_REARM_POLLS as usize + 8) {
            let _ = drv.is_card_present();
            if drv.transport_mut().sent.len() > settled {
                let w = &drv.transport_mut().sent[settled];
                assert_eq!(
                    w.as_slice(),
                    reader::RF_DEACTIVATE_DISCOVERY.as_slice(),
                    "parked re-arm step 1 is the #105 deactivate"
                );
                rearmed = true;
                break;
            }
            assert!(
                drv.transport_mut().sent.len() <= settled,
                "poll {i}: unexpected write before the parked cadence"
            );
        }
        assert!(rearmed, "parked driver must still re-arm (slow cadence)");

        // A tag NTF in the collect window restores presence + unparks.
        drv.transport_mut().push_notification(&discover_ntf());
        assert!(
            drv.is_card_present(),
            "tag NTF during the parked re-arm must restore presence"
        );
        assert!(!drv.discovery_parked, "latch must clear the park");
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

        // Park-in-discovery contract (bench 2026-10-12): re-arm cycles
        // run only until absence is confirmed — ABSENT_REARMS_TO_CLEAR
        // empty cycles clear the stale tag and PARK the cadence (the
        // perpetual cadence semi-muted the PN7160 after ~11 cycles).
        for cycle in 0..ABSENT_REARMS_TO_CLEAR {
            run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
            assert_eq!(
                drv.transport_mut().transacts,
                0,
                "cycle {}: presence path must never block on a transact",
                cycle
            );
        }
        assert!(!drv.is_card_present(), "absence confirmed by now");

        // Parked: polls drain only — zero writes, the strongest form of
        // the at-most-once invariant.
        let writes = drv.transport_mut().sent.len();
        for _ in 0..(PRESENCE_REARM_POLLS as usize * 3) {
            let _ = drv.is_card_present();
        }
        assert_eq!(
            drv.transport_mut().sent.len(),
            writes,
            "parked driver must not write"
        );

        // Un-park on arrival: a fresh tag's NTF latches presence through
        // the plain drain, and the cadence is armed again for the next
        // removal.
        drv.transport_mut().push_notification(&discover_ntf());
        assert!(drv.is_card_present(), "parked driver still sees new tags");
        run_rearm_cycle(&mut drv, &deactivate_discovery_rsp_ok());
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

        // Window expires without a tag: one empty cycle, presence held —
        // and the armed #125 probe fires (the boot latch armed it).
        assert!(drv.is_card_present());
        assert!(drv.is_card_present());
        assert_eq!(
            drv.rearm_phase(),
            Some(RearmPhase::DiscoverProbeRsp { polls: 0 })
        );
        // Probe RSP never arrives either: it gives up at its deadline,
        // keeping presence.
        assert!(drv.is_card_present());
        assert!(drv.is_card_present());
        assert_eq!(drv.rearm_phase(), None);
        assert!(drv.is_card_present());

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
        let t = MockTransport::new();
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

        // Many polls must not WRITE the transport (no re-arm during a
        // session — it would tear the RF interface down). Since issue
        // #109 they do drain (read-only) to catch mid-session removal,
        // which never sends a command.
        let sent_before = drv.transport_mut().sent.len();
        for _ in 0..(PRESENCE_REARM_POLLS * 3) {
            assert!(drv.is_card_present());
        }
        assert_eq!(drv.transport_mut().sent.len(), sent_before);
    }
}
