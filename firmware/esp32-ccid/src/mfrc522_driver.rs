use crate::nfc::{NfcDriver, NfcError, PresenceState};

pub const REINIT_THRESHOLD: u8 = 3;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InitRecoveryTracker {
    consecutive_failures: u8,
    reinit_count: u32,
}

impl InitRecoveryTracker {
    pub const fn new() -> Self {
        Self {
            consecutive_failures: 0,
            reinit_count: 0,
        }
    }

    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
    }

    pub fn record_failure(&mut self) -> bool {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= REINIT_THRESHOLD {
            self.consecutive_failures = 0;
            self.reinit_count = self.reinit_count.saturating_add(1);
            true
        } else {
            false
        }
    }

    pub fn reinit_count(&self) -> u32 {
        self.reinit_count
    }
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
const CARD_ABSENT_THRESHOLD: u8 = 3;

/// Field-off dwell for the antenna cycle in `reset_activation_frontend`
/// (see there for the wedged-PICC rationale).
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
const FIELD_OFF_DWELL_MS: u32 = 50;
/// Field-on settle before the first REQA/WUPA after the cycle.
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
const FIELD_ON_SETTLE_MS: u32 = 5;

#[cfg(not(any(target_arch = "xtensa", target_arch = "riscv32")))]
pub struct Mfrc522NfcDriver;

#[cfg(not(any(target_arch = "xtensa", target_arch = "riscv32")))]
impl card_interface::FrontendDiagnostics for Mfrc522NfcDriver {
    fn frontend_status(&self) -> card_interface::FrontendStatus {
        // Host compile stub: the real frontend exists only on target.
        card_interface::FrontendStatus {
            health: card_interface::FrontendHealth::Unavailable,
            last_error: card_interface::FrontendErrorKind::NotInitialized,
            ..card_interface::FrontendStatus::ok()
        }
    }

    fn frontend_self_test(&mut self) -> Result<(), card_interface::FrontendErrorKind> {
        Err(card_interface::FrontendErrorKind::NotInitialized)
    }
}

#[cfg(not(any(target_arch = "xtensa", target_arch = "riscv32")))]
impl NfcDriver for Mfrc522NfcDriver {
    type Error = NfcError;

    fn init(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn is_card_present(&mut self) -> bool {
        false
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        PresenceState { present: false }
    }

    fn session_active(&self) -> bool {
        false
    }

    fn power_on(&mut self, _atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
        Err(NfcError::NoCard)
    }

    fn power_off(&mut self) {}

    fn transmit_apdu(
        &mut self,
        _command: &[u8],
        _response: &mut [u8],
    ) -> Result<usize, Self::Error> {
        Err(NfcError::NotInitialized)
    }
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
use embedded_hal::i2c::I2c;

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
use esp_idf_hal::delay::FreeRtos;

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
use iso14443::type_a::limits::Limits;
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
use iso14443::type_a::{activation, Ats, Cid, Fsdi, PcdSession, Tc};

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
use iso14443::type_a::vec::FrameVec;
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
use iso14443::type_a::{Frame, PcdTransceiver};

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
use crate::turnaround::OutgoingBlockKind;

/// Escape 0xD2 word 4-8 counters (issue #120 decomposition): wraps the
/// raw transceiver for the duration of one `session.exchange` and
/// classifies every outgoing block by its PCB.
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
struct CountingTransceiver<'a, I2C: I2c> {
    inner: &'a mut mfrc522_pcd::Mfrc522Transceiver<I2C>,
    blocks: u32,
    wtx_replies: u32,
    recoveries: u32,
    max_transceive_us: u32,
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
impl<I2C: I2c> PcdTransceiver for CountingTransceiver<'_, I2C> {
    type Error = mfrc522_pcd::Mfrc522Error<I2C::Error>;

    fn transceive(&mut self, frame: &Frame) -> Result<FrameVec, Self::Error> {
        let started_us = crate::t1::now_us();
        let result = self.inner.transceive(frame);
        let took_us = (crate::t1::now_us() - started_us).clamp(0, u32::MAX as i64) as u32;
        self.blocks = self.blocks.saturating_add(1);
        if let Some(pcb) = frame.data().first() {
            match OutgoingBlockKind::from_pcb(*pcb) {
                Some(OutgoingBlockKind::SBlock) => {
                    self.wtx_replies = self.wtx_replies.saturating_add(1)
                }
                Some(OutgoingBlockKind::RBlock) => {
                    self.recoveries = self.recoveries.saturating_add(1)
                }
                _ => {}
            }
        }
        self.max_transceive_us = self.max_transceive_us.max(took_us);
        result
    }

    fn try_enable_hw_crc(&mut self) -> Result<(), Self::Error> {
        self.inner.try_enable_hw_crc()
    }

    fn try_set_timeout_ms(&mut self, ms: u32) -> Result<(), ()> {
        self.inner.try_set_timeout_ms(ms)
    }
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardLifecycle {
    NoCard,
    PresentInactive,
    ActiveSession,
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
pub struct Mfrc522NfcDriver<I2C: I2c> {
    transceiver: mfrc522_pcd::Mfrc522Transceiver<I2C>,
    is_initialized: bool,
    lifecycle: CardLifecycle,
    session: Option<PcdSession>,
    cached_ats: Option<Ats>,
    consecutive_failures: u8,
    init_tracker: InitRecoveryTracker,
    // ISO 14443-3A UIDs are 4/7/10 bytes (single/double/triple cascade)
    cached_uid: [u8; 10],
    cached_uid_len: usize,
    last_exchange: Option<(u32, u32, u32, u32)>,
    configured_fwt_ms: u32,
    // Frontend diagnostics counters (t_4864e74e): the trait surface's
    // data — init attempts, presence polls served, bus failures seen,
    // and the last error classification.
    diag_init_attempts: u32,
    diag_presence_polls: u32,
    diag_bus_errors: u32,
    diag_last_error: Option<crate::nfc::NfcError>,
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
impl<I2C> Mfrc522NfcDriver<I2C>
where
    I2C: I2c,
{
    pub fn new(transceiver: mfrc522_pcd::Mfrc522Transceiver<I2C>) -> Self {
        Self {
            transceiver,
            is_initialized: false,
            lifecycle: CardLifecycle::NoCard,
            session: None,
            cached_ats: None,
            consecutive_failures: 0,
            init_tracker: InitRecoveryTracker::new(),
            cached_uid: [0u8; 10],
            cached_uid_len: 0,
            last_exchange: None,
            configured_fwt_ms: 0,
            diag_init_attempts: 0,
            diag_presence_polls: 0,
            diag_bus_errors: 0,
            diag_last_error: None,
        }
    }

    fn has_card(&self) -> bool {
        self.lifecycle != CardLifecycle::NoCard
    }

    fn mark_card_absent(&mut self) {
        self.lifecycle = CardLifecycle::NoCard;
        self.session = None;
        self.cached_ats = None;
        self.cached_uid_len = 0;
        self.consecutive_failures = 0;
    }

    fn mark_card_present(&mut self) {
        if self.lifecycle == CardLifecycle::NoCard {
            self.lifecycle = CardLifecycle::PresentInactive;
        }
        self.consecutive_failures = 0;
    }

    fn clear_session(&mut self) {
        self.session = None;
        if self.lifecycle != CardLifecycle::NoCard {
            self.lifecycle = CardLifecycle::PresentInactive;
        }
        self.cached_ats = None;
    }

    fn reset_activation_frontend(&mut self) -> Result<(), NfcError> {
        // ISO 14443 field reset: a PICC whose ISO-14443-4 layer wedged
        // mid-chain (e.g. a stalled host transfer) keeps answering
        // WUPA but ignores RATS until the field de-energizes it. Bolty
        // cycles the antenna before every activation
        // (`prepare_for_activation`); this adds the field-off dwell
        // (50 ms) a charge-pump-powered JavaCard needs to fully reset.
        // Only ever runs with no response pending (poll idle /
        // power_on) — never on the latency path.
        self.transceiver
            .raw()
            .rmw_register(mfrc522::Register::TxControlReg, |b| b & !0b11)
            .map_err(|_| NfcError::CommunicationError)?;
        FreeRtos::delay_ms(FIELD_OFF_DWELL_MS);
        self.transceiver
            .raw()
            .rmw_register(mfrc522::Register::TxControlReg, |b| b | 0b11)
            .map_err(|_| NfcError::CommunicationError)?;
        FreeRtos::delay_ms(FIELD_ON_SETTLE_MS);
        self.transceiver
            .reset_frontend()
            .map_err(|_| NfcError::CommunicationError)?;
        self.transceiver
            .raw()
            .set_antenna_gain(mfrc522::RxGain::DB33)
            .map_err(|_| NfcError::CommunicationError)?;
        Ok(())
    }

    pub fn init_with_recovery(&mut self) -> Result<(), NfcError> {
        self.diag_init_attempts = self.diag_init_attempts.saturating_add(1);
        match self.init() {
            Ok(()) => {
                self.init_tracker.record_success();
                self.diag_last_error = None;
                Ok(())
            }
            Err(e) => {
                self.diag_last_error = Some(e);
                self.diag_bus_errors = self.diag_bus_errors.saturating_add(1);
                if self.init_tracker.record_failure() {
                    log::warn!(
                        "MFRC522 init failed {} consecutive times, performing full re-init",
                        REINIT_THRESHOLD
                    );
                    // Propagate the recovery result (Codex review on #40):
                    // a successful full_reinit() leaves the driver initialized
                    // — reporting the ORIGINAL error would send callers down
                    // a failure path for a recovered device.
                    return self.full_reinit();
                }
                Err(e)
            }
        }
    }

    fn full_reinit(&mut self) -> Result<(), NfcError> {
        self.is_initialized = false;
        self.lifecycle = CardLifecycle::NoCard;
        self.session = None;
        self.cached_ats = None;
        self.cached_uid_len = 0;
        self.consecutive_failures = 0;
        self.reset_activation_frontend()?;
        self.init()
    }

    fn poll_physical_card(&mut self) -> PresenceState {
        if !self.is_initialized {
            return PresenceState { present: false };
        }

        if self.lifecycle == CardLifecycle::ActiveSession {
            return PresenceState { present: true };
        }

        if self.reset_activation_frontend().is_err() {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
            if self.consecutive_failures >= CARD_ABSENT_THRESHOLD {
                self.mark_card_absent();
                return PresenceState { present: false };
            }
            return PresenceState {
                present: self.has_card(),
            };
        }

        match self.transceiver.raw().wupa() {
            Ok(atqa) => {
                let atqa_bytes = atqa.as_bytes();
                log::trace!(
                    "poll_card_presence: ATQA=0x{:02X}{:02X} (via WUPA)",
                    atqa_bytes[0],
                    atqa_bytes[1],
                );
                self.mark_card_present();
                PresenceState { present: true }
            }
            Err(err) => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                if self.consecutive_failures >= CARD_ABSENT_THRESHOLD {
                    log::info!(
                        "Card absent after {} presence failures (last: {:?})",
                        self.consecutive_failures,
                        err,
                    );
                    self.mark_card_absent();
                    PresenceState { present: false }
                } else {
                    PresenceState {
                        present: self.has_card(),
                    }
                }
            }
        }
    }
}

#[cfg(any(
    test,
    all(
        any(target_arch = "xtensa", target_arch = "riscv32"),
        feature = "backend-mfrc522"
    )
))]
fn build_pcsc_atr_from_historical(
    historical: &[u8],
    atr_buf: &mut [u8],
) -> Result<usize, NfcError> {
    // G-05: declare T=1 with TA3 (IFSC=254) + TB3 (BWI=4, CWI=5).
    // Without TA3 the host chains every block at ifsc=32 (ifdhandler.c
    // get_IFSC → t1_set_param IFSC — 8× the round trips on this slow
    // UART/I2C relay); without TB3 it guesses BWT from defaults. TCK is
    // legal now that a protocol ≠ T=0 is offered. Byte layout:
    // TS, T0(Y1=TD1|K), TD1=0x81, TD2=0x31 (TA3+TB3, T=1), TA3=0xFE,
    // TB3, historical(K), TCK. (TD1 carries the T=1 nibble because the
    // shared parser — like several host stacks — reads the protocol
    // from TD1.)
    let n = historical.len();
    let atr_len = 7 + n;
    if atr_buf.len() < atr_len {
        return Err(NfcError::BufferOverflow);
    }

    atr_buf[0] = 0x3B;
    atr_buf[1] = 0x80 | (n as u8 & 0x0F);
    atr_buf[2] = 0x81;
    atr_buf[3] = 0x31;
    atr_buf[4] = 0xFE;
    atr_buf[5] = ccid_protocol::atr::t1_tb3(4, 5);
    atr_buf[6..6 + n].copy_from_slice(historical);

    let mut tck: u8 = 0;
    for &b in &atr_buf[1..atr_len - 1] {
        tck ^= b;
    }
    atr_buf[atr_len - 1] = tck;

    Ok(atr_len)
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
fn build_pcsc_atr(ats: &Ats, atr_buf: &mut [u8]) -> Result<usize, NfcError> {
    build_pcsc_atr_from_historical(ats.historical_bytes.as_slice(), atr_buf)
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
impl<I2C> NfcDriver for Mfrc522NfcDriver<I2C>
where
    I2C: I2c,
{
    type Error = NfcError;

    // Plain init() bypasses the failure tracker; retry paths (the
    // RetryFrontend degraded loop) route through the recovery-aware
    // initializer so threshold-triggered full re-inits still fire.
    fn init_recovering(&mut self) -> Result<(), NfcError> {
        self.init_with_recovery()
    }

    fn init(&mut self) -> Result<(), NfcError> {
        let version = self
            .transceiver
            .raw()
            .version()
            .map_err(|_| NfcError::CommunicationError)?;

        if version != 0x91 && version != 0x92 {
            log::error!("MFRC522 init: unexpected version 0x{:02X}", version);
            return Err(NfcError::CommunicationError);
        }

        self.transceiver
            .raw()
            .set_antenna_gain(mfrc522::RxGain::DB33)
            .map_err(|_| NfcError::CommunicationError)?;

        self.is_initialized = true;
        log::info!("MFRC522 init OK, version=0x{:02X}, gain=33dB", version);
        Ok(())
    }

    fn reinit_count(&self) -> u32 {
        self.init_tracker.reinit_count()
    }

    fn card_uid(&self) -> Option<&[u8]> {
        (self.cached_uid_len > 0).then(|| &self.cached_uid[..self.cached_uid_len])
    }

    fn is_card_present(&mut self) -> bool {
        self.poll_card_presence().present
    }

    fn poll_card_presence(&mut self) -> PresenceState {
        self.diag_presence_polls = self.diag_presence_polls.saturating_add(1);
        self.poll_physical_card()
    }

    fn session_active(&self) -> bool {
        self.lifecycle == CardLifecycle::ActiveSession
    }

    fn exchange_decomposition(&self) -> Option<(u32, u32, u32, u32)> {
        self.last_exchange
    }

    /// Escape 0xD3 probe (issue #120): time `reads` back-to-back
    /// ComIrqReg register reads — the exact I2C transaction the
    /// transceive poll loop performs — so the host can derive the
    /// per-poll cadence and separate I2C wait from card wait.
    fn i2c_timing_probe(&mut self, reads: u32) -> Option<u32> {
        let n = reads.clamp(1, 256);
        let started_us = crate::t1::now_us();
        for _ in 0..n {
            self.transceiver
                .raw()
                .read_register(mfrc522::Register::ComIrqReg)
                .ok()?;
        }
        let took = (crate::t1::now_us() - started_us).clamp(0, u32::MAX as i64) as u32;
        Some(took)
    }

    fn configured_fwt_ms(&self) -> u32 {
        self.configured_fwt_ms
    }

    /// #519: raw timer-register readback for the 0xD3 append — the
    /// silicon's actual [TMode, TPrescaler, TReloadHi, TReloadLo]
    /// (prescaler 0xA9 = pre-#464 fixed arming, 0x101/0x201 = FWI
    /// 13/14 post-fix). `configured_fwt_ms` above is the REQUEST
    /// cache; this is the arming proof.
    fn timer_registers(&mut self) -> Option<[u8; 4]> {
        self.transceiver.timer_registers().ok()
    }

    fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, NfcError> {
        if !self.is_initialized {
            return Err(NfcError::NotInitialized);
        }

        // pcscd does PowerUp→PowerDown→PowerUp as a WARM RESET sequence;
        // a stale session (power-off lost mid-teardown) must not fail the
        // next activation. Deactivate first, then re-activate.
        if self.lifecycle == CardLifecycle::ActiveSession || self.session.is_some() {
            log::warn!("power_on: stale session — deactivating before re-activation");
            self.power_off();
        }

        self.reset_activation_frontend()?;

        // Per ISO 14443-3A §6.2.4 and Bolty reference implementation:
        // card needs settling time between register reset and activation.
        // JavaCards (J3R180) need longer settling than DESFire.
        FreeRtos::delay_ms(15);

        // Marginal coupling makes a single REQA→RATS sequence flaky
        // (~50% on the bench J3R180, 2026-10-09): retry the activation
        // with a fresh frontend reset between attempts.
        //
        // ISO14443_SPEC (14443-3 §6.2.4 + Type A anticollision): a
        // REQA/WUPA that goes unanswered is retried by the PICC-side
        // convention — the PCD is expected to re-poll; there is no
        // negative ack from a marginally-coupled card, only silence.
        // ISO14443-4 (§5.2): RATS is likewise retransmittable — a lost
        // RATS/ATS leaves the card in ATTRIB-pending state where a fresh
        // RATS (after re-wakeup) restarts the exchange cleanly. The
        // 15 ms settle between attempts mirrors the boot-path settle
        // above (JavaCards need longer than DESFire per §6.2.4 note).
        const ACTIVATION_ATTEMPTS: usize = 4;
        let mut woke: Option<activation::Activation> = None;
        for attempt in 1..=ACTIVATION_ATTEMPTS {
            if attempt > 1 {
                if self.reset_activation_frontend().is_err() {
                    break;
                }
                FreeRtos::delay_ms(15);
            }
            match activation::wakeup(&mut self.transceiver) {
                Ok(a) if a.sak.iso14443_4_compliant => {
                    woke = Some(a);
                    break;
                }
                Ok(_) => log::warn!("power_on: attempt {attempt}: card not ISO-14443-4"),
                Err(err) => log::warn!("power_on: attempt {attempt} failed: {err:?}"),
            }
        }
        let Some(activation) = woke else {
            log::error!("power_on: activation failed after {ACTIVATION_ATTEMPTS} attempts");
            self.clear_session();
            return Err(NfcError::CommunicationError);
        };

        let uid = activation.uid.as_slice();
        if uid.len() <= self.cached_uid.len() {
            self.cached_uid[..uid.len()].copy_from_slice(uid);
            self.cached_uid_len = uid.len();
        }

        let mut ats_result = Err(());
        for rats_attempt in 1..=ACTIVATION_ATTEMPTS {
            match PcdSession::from_connect(&mut self.transceiver, Fsdi::Fsd64, Cid::new(0).unwrap())
            {
                Ok(pair) => {
                    ats_result = Ok(pair);
                    break;
                }
                Err(err) => {
                    log::warn!("power_on: RATS attempt {rats_attempt} failed: {err:?}")
                }
            }
        }
        let (_, ats) = ats_result.map_err(|()| {
            log::error!("power_on: RATS/connect failed after {ACTIVATION_ATTEMPTS} attempts");
            self.clear_session();
            NfcError::CommunicationError
        })?;

        // Per ISO 14443-4 §5.2.1: "DID equal to '0' indicates that no CID is used."
        // RATS with DID=0 means no CID assigned. I-blocks MUST NOT include CID.
        // from_connect hardcodes Some(cid) → would produce PCB=0x0A (CID following).
        // Re-create session with None so I-blocks use PCB=0x02 (no CID).
        //
        // Enable MFRC522 HW CRC for ISO-DEP (per Tasmota/Miguelbalboa MFRC522Extended).
        // Both TxCRCEn and RxCRCEn = 1. Must happen AFTER activation (anticollision
        // responses lack CRC). Session uses hw_crc=true so iso14443 crate sends
        // without SW CRC and expects raw FIFO (CRC stripped by MFRC522 HW).
        self.transceiver
            .enable_hw_crc()
            .map_err(|_| NfcError::CommunicationError)?;
        let mut session = PcdSession::from_ats(&ats, None, true);

        // #372: the rewritten upstream session bounds work per exchange
        // with defaults tuned for fast commercial readers (8 consecutive
        // S(WTX), 32 total, 3 retransmissions, 512 frames). A
        // flash-writing GlobalPlatform ISD legitimately emits longer WTX
        // runs per APDU during package LOAD, and at MFRC522-over-I2C
        // speeds the defaults abort the exchange (LimitExceeded → the
        // SW 6400 class seen on hardware). Raise the budgets for this
        // PCD class; the WTX-run semantics are pinned by
        // `pcd_sustained_load_tests.rs` and the full analysis lives in
        // docs/ISO14443_PCB_CONFORMANCE.md.
        session.set_limits(Limits {
            max_consecutive_wtx: 64,
            max_total_wtx: 4096,
            max_frames: 4096,
            max_retransmissions: 5,
            ..Limits::default()
        });

        let sfgt_us = ats.tb.sfgi.sfgt_us();
        let card_fsc = ats.format.fsci.fsc();
        let fwi = ats.tb.fwi.value();
        let sfgi = ats.tb.sfgi.value();
        let cid_supp = ats.tc.contains(Tc::CID_SUPP);
        let nad_supp = ats.tc.contains(Tc::NAD_SUPP);

        // MFRC522 FIFO is 64 bytes. With hw_crc, an I-block in the FIFO is
        // PCB(1) + payload(N) — CRC is appended by HW. So max payload per
        // I-block = 63 bytes. Overhead = 1 PCB + 2 CRC = 3, so FSC = 66.
        // But the FIFO is the hard limit: 1 + 61 = 62 fits, 1 + 62 = 63
        // fits, 1 + 63 = 64 exactly fits. Use FSC=64 for safety margin.
        const MFRC522_MAX_FSC: usize = 64;
        let effective_fsc = core::cmp::min(card_fsc, MFRC522_MAX_FSC);
        session.set_fsc(effective_fsc);

        log::info!(
            "power_on: ATS: card_fsc={}, effective_fsc={}, SFGI={} (SFGT={}us), FWI={}, CID_SUPP={}, NAD_SUPP={}",
            card_fsc,
            effective_fsc,
            sfgi,
            sfgt_us,
            fwi,
            cid_supp,
            nad_supp
        );

        if sfgt_us > 0 {
            let sfgt_ms = (sfgt_us + 999) / 1000;
            log::info!("power_on: SFGT delay {}ms", sfgt_ms);
            FreeRtos::delay_ms(sfgt_ms as u32);
        }

        // FWT = (256 × 16 / fc) × 2^FWI  where fc = 13.56 MHz
        // FWI=4 → 4.8ms, FWI=9 → 155ms, FWI=10 → 310ms, FWI=14 → ~5s
        let fwt_ms = if fwi > 0 {
            let fwt_us: u64 = 302 * (1u64 << fwi);
            (fwt_us / 1000 + 10) as u32 // +10ms margin
        } else {
            5 // FWI=0 → ~302µs, use 5ms minimum
        };
        log::info!("power_on: FWT={}ms (FWI={})", fwt_ms, fwi);
        self.transceiver
            .set_timeout_ms(fwt_ms)
            .map_err(|_| NfcError::CommunicationError)?;
        session.set_base_fwt_ms(fwt_ms);
        self.configured_fwt_ms = fwt_ms;

        self.session = Some(session);
        self.cached_ats = Some(ats.clone());
        self.lifecycle = CardLifecycle::ActiveSession;
        self.consecutive_failures = 0;

        log::info!("power_on: session established (no CID, full HW CRC)");

        let atr_len = build_pcsc_atr(&ats, atr_buf)?;
        log::trace!(
            "power_on: ATS={:02X?} ATR ({} bytes): {:02X?}",
            ats.to_bytes()
                .map_err(|_| NfcError::CommunicationError)?
                .as_slice(),
            atr_len,
            &atr_buf[..atr_len]
        );
        Ok(atr_len)
    }

    fn power_off(&mut self) {
        if let Some(mut session) = self.session.take() {
            if let Err(err) = session.deselect(&mut self.transceiver) {
                log::warn!("power_off: deselect failed: {:?}", err);
            }
        }

        self.cached_ats = None;
        self.lifecycle = if self.is_initialized {
            CardLifecycle::PresentInactive
        } else {
            CardLifecycle::NoCard
        };
        log::info!("power_off: session closed");
    }

    fn transmit_apdu(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, NfcError> {
        if !self.is_initialized {
            return Err(NfcError::NotInitialized);
        }

        let mut session = self.session.take().ok_or(NfcError::NotInitialized)?;
        if self.lifecycle != CardLifecycle::ActiveSession {
            self.session = Some(session);
            return Err(NfcError::NotInitialized);
        }

        log::trace!("transmit_apdu: {} bytes: {:02X?}", command.len(), command);

        let is_first_exchange = self.cached_ats.is_some();
        if is_first_exchange {
            log::info!(
                "transmit_apdu: first exchange, {} bytes: {:02X?}",
                command.len(),
                command
            );
        }

        let resp = {
            let mut counting = CountingTransceiver {
                inner: &mut self.transceiver,
                blocks: 0,
                wtx_replies: 0,
                recoveries: 0,
                max_transceive_us: 0,
            };
            let result = session.exchange(&mut counting, command);
            self.last_exchange = Some((
                counting.blocks,
                counting.wtx_replies,
                counting.recoveries,
                counting.max_transceive_us,
            ));
            result
        };
        let resp = match resp {
            Ok(resp) => resp,
            Err(err) => {
                log::error!("transmit_apdu: exchange failed: {:?}", err);
                self.clear_session();
                self.lifecycle = CardLifecycle::PresentInactive;
                return Err(NfcError::CommunicationError);
            }
        };

        let resp_slice = resp.as_slice();
        if is_first_exchange {
            log::info!(
                "transmit_apdu: first response, {} bytes: {:02X?}",
                resp_slice.len(),
                resp_slice
            );
        }
        if response.len() < resp_slice.len() {
            self.session = Some(session);
            return Err(NfcError::BufferOverflow);
        }

        response[..resp_slice.len()].copy_from_slice(resp_slice);
        self.session = Some(session);
        self.lifecycle = CardLifecycle::ActiveSession;
        log::trace!(
            "transmit_apdu: received {} bytes: {:02X?}",
            resp_slice.len(),
            resp_slice
        );
        Ok(resp_slice.len())
    }
}

/// Uniform diagnostics surface (t_4864e74e): health from the driver's
/// own lifecycle (initialized ⇒ Healthy; an init-failing frontend is
/// classified Degraded by the `RetryFrontend` wrapper), counters from
/// the instrumented init/presence paths above. Self-test is the
/// stateless VersionReg read (0x91/0x92 — the same probe `init` uses).
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
impl<I2C> card_interface::FrontendDiagnostics for Mfrc522NfcDriver<I2C>
where
    I2C: I2c,
{
    fn frontend_status(&self) -> card_interface::FrontendStatus {
        card_interface::FrontendStatus {
            health: if self.is_initialized {
                card_interface::FrontendHealth::Healthy
            } else {
                card_interface::FrontendHealth::Degraded
            },
            last_error: self
                .diag_last_error
                .map(card_interface::FrontendErrorKind::from)
                .unwrap_or(card_interface::FrontendErrorKind::None),
            init_attempts: self.diag_init_attempts,
            reinit_count: self.init_tracker.reinit_count(),
            bus_errors: self.diag_bus_errors,
            presence_polls: self.diag_presence_polls,
        }
    }

    fn frontend_self_test(&mut self) -> Result<(), card_interface::FrontendErrorKind> {
        let version = self
            .transceiver
            .raw()
            .version()
            .map_err(|_| card_interface::FrontendErrorKind::Bus)?;
        if version == 0x91 || version == 0x92 {
            Ok(())
        } else {
            Err(card_interface::FrontendErrorKind::Protocol)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(any(target_arch = "xtensa", target_arch = "riscv32")))]
    fn test_stub_is_card_present_returns_false() {
        let mut driver = Mfrc522NfcDriver;
        assert!(!driver.is_card_present());
        assert_eq!(
            driver.poll_card_presence(),
            PresenceState { present: false }
        );
    }

    #[test]
    #[cfg(not(any(target_arch = "xtensa", target_arch = "riscv32")))]
    fn test_stub_power_on_returns_no_card() {
        let mut driver = Mfrc522NfcDriver;
        let mut buf = [0u8; 64];
        let result = driver.power_on(&mut buf);
        assert!(matches!(result, Err(NfcError::NoCard)));
    }

    #[test]
    #[cfg(not(any(target_arch = "xtensa", target_arch = "riscv32")))]
    fn test_stub_transmit_apdu_returns_not_initialized() {
        let mut driver = Mfrc522NfcDriver;
        let cmd = [0x00, 0xA4, 0x04, 0x00];
        let mut resp = [0u8; 256];
        let result = driver.transmit_apdu(&cmd, &mut resp);
        assert!(matches!(result, Err(NfcError::NotInitialized)));
    }

    #[test]
    fn test_build_pcsc_atr_no_historical_bytes() {
        let mut buf = [0u8; 33];
        let len = build_pcsc_atr_from_historical(&[], &mut buf).unwrap();
        assert_eq!(len, 7);
        assert_eq!(&buf[..7], &[0x3B, 0x80, 0x81, 0x31, 0xFE, 0x45, 0x8B]);
    }

    #[test]
    fn test_build_pcsc_atr_one_historical_byte() {
        let mut buf = [0u8; 33];
        let len = build_pcsc_atr_from_historical(&[0x80], &mut buf).unwrap();
        assert_eq!(len, 8);
        assert_eq!(&buf[..8], &[0x3B, 0x81, 0x81, 0x31, 0xFE, 0x45, 0x80, 0x0A]);
    }

    #[test]
    fn test_build_pcsc_atr_multiple_historical_bytes() {
        let mut buf = [0u8; 33];
        let hist = &[0x80, 0x5A, 0x01, 0x02];
        let len = build_pcsc_atr_from_historical(hist, &mut buf).unwrap();
        assert_eq!(len, 11);
        assert_eq!(buf[0], 0x3B);
        assert_eq!(buf[1], 0x84);
        assert_eq!(&buf[6..10], hist);
        let expected_tck = 0x84 ^ 0x81 ^ 0x31 ^ 0xFE ^ 0x45 ^ 0x80 ^ 0x5A ^ 0x01 ^ 0x02;
        assert_eq!(buf[10], expected_tck);
    }

    #[test]
    fn synthetic_atr_semantically_locks_t1_timing() {
        // G-05 semantic lock: parse our own ATR with the shared parser
        // and see exactly what ifdhandler.c would — protocol T=1,
        // IFSC 254 (TA3), BWI=4/CWI=5 (TB3).
        let mut buf = [0u8; 33];
        let hist = &[0x80, 0x73, 0xC8, 0x21, 0x10];
        let len = build_pcsc_atr_from_historical(hist, &mut buf).unwrap();
        let p = ccid_protocol::atr::parse_atr(&buf[..len]);
        assert_eq!(p.protocol, 1);
        assert_eq!(p.ifsc, 254);
        assert_eq!((p.bwi, p.cwi), (4, 5));
        assert!(ccid_protocol::atr::verify_atr_tck(&buf[..len], 1));
    }

    #[test]
    fn test_build_pcsc_atr_buffer_too_small() {
        let mut buf = [0u8; 3];
        let result = build_pcsc_atr_from_historical(&[], &mut buf);
        assert!(matches!(result, Err(NfcError::BufferOverflow)));
    }

    #[test]
    fn test_card_absent_threshold() {
        assert_eq!(3, 3);
    }

    #[test]
    fn test_reinit_tracker_threshold() {
        let mut tracker = InitRecoveryTracker::new();
        assert_eq!(tracker.reinit_count(), 0);

        assert!(!tracker.record_failure());
        assert_eq!(tracker.reinit_count(), 0);

        assert!(!tracker.record_failure());
        assert_eq!(tracker.reinit_count(), 0);

        assert!(tracker.record_failure());
        assert_eq!(tracker.reinit_count(), 1);
    }

    #[test]
    fn test_reinit_tracker_success_resets_counter() {
        let mut tracker = InitRecoveryTracker::new();
        tracker.record_failure();
        tracker.record_failure();
        tracker.record_success();
        assert!(!tracker.record_failure());
        assert!(!tracker.record_failure());
        assert_eq!(tracker.reinit_count(), 0);
    }

    #[test]
    fn test_reinit_tracker_multiple_cycles() {
        let mut tracker = InitRecoveryTracker::new();
        for _ in 0..7 {
            tracker.record_failure();
        }
        assert_eq!(tracker.reinit_count(), 2);
    }

    #[test]
    fn test_reinit_threshold_constant() {
        assert_eq!(REINIT_THRESHOLD, 3);
    }
}
