//! Pure embedded-hal PN7160 transport (kanban t_4864e74e — the
//! boards-as-data §3.1 constructor seam's board-blind half).
//!
//! [`HalPn7160Transport`] implements the same [`Transport`] contract as
//! the bench-proven `EspPn7160Transport` (esp-idf FFI, opt-in `esp`
//! feature) but over the *pure* embedded-hal 1.0 traits only — a bus
//! handle (`I2c`), an IRQ pin (`InputPin`), a VEN pin (`OutputPin`) and
//! a delay (`DelayNs`). Zero board knowledge: the only data fact it
//! carries is the controller's I2C address; every pin/bus/speed number
//! stays with the caller (the Layer-B constructor in the firmware bin
//! resolves those from a `boards::BoardDesc` table).
//!
//! Host-testable by construction: the whole module compiles and tests
//! on the host against mock embedded-hal pieces, and any backend whose
//! HAL implements the 1.0 traits (esp-idf-hal included) can drive the
//! real chip through it without touching this crate's esp bindings.
//!
//! Framing and reply-routing rules are byte-identical to the esp
//! transport's contract: two-phase NCI read (3-byte header, then
//! payload), IRQ-gated non-blocking polls, and the ONE shared
//! non-NTF-first reply decision ([`crate::is_transact_reply`]).

use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::i2c::I2c;
use heapless::Deque;

use crate::{is_transact_reply, Frame, Transport, NTF_STASH_SLOTS};

/// VEN power-cycle timings, byte-exact vs the esp transport's
/// `ven_cycle` (itself byte-exact vs the proven nci.c:77-86): assert
/// VEN, 10 ms hold, drop, 50 ms off, raise, 50 ms settle — the
/// ACK-window opener the PN7160 bring-up depends on.
const VEN_HOLD_MS: u32 = 10;
const VEN_OFF_MS: u32 = 50;
const VEN_SETTLE_MS: u32 = 50;
/// IRQ wait budget for `transact` (µs-class counting is not portable
/// over `DelayNs`; the budget is counted in 1 ms delay ticks —
/// `delay_ms` is at-least semantics, same as FreeRtos).
const IRQ_WAIT_BUDGET_MS: u32 = 200;

/// Pure embedded-hal link failure vocabulary — deliberately coarse
/// (the driver-level diagnostics classify through
/// `card_interface::FrontendErrorKind` one layer up, in the firmware).
/// #257 error-model pass: `Display`/`core::error::Error` derived —
/// additive only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HalLinkError {
    /// The controller did not ACK the post-VEN-rise probe (chip mute,
    /// wrong address strap, or dead bus).
    #[error("controller did not ACK the post-VEN-rise probe")]
    NoAckAfterVenCycle,
}

/// PN7160 NCI transport over pure embedded-hal 1.0 handles (t_4864e74e).
///
/// Constructed from an already-configured I2C bus (speed, pull-ups —
/// bus facts the caller owns), the IRQ/VEN pins and a delay source.
/// Bring-up (VEN power cycle + in-window probe) lives in
/// [`bring_up`](Self::bring_up), NOT at the caller's call site.
pub struct HalPn7160Transport<I2C, IRQ, VEN, DELAY> {
    i2c: I2C,
    irq: IRQ,
    ven: VEN,
    delay: DELAY,
    addr: u8,
    /// Transact NTF stash — drop-oldest ring, same policy and capacity
    /// as the esp transport and the host mock (ONE capacity definition,
    /// [`NTF_STASH_SLOTS`]).
    ntf: Deque<Frame, NTF_STASH_SLOTS>,
}

impl<I2C, IRQ, VEN, DELAY> HalPn7160Transport<I2C, IRQ, VEN, DELAY>
where
    I2C: I2c,
    IRQ: InputPin,
    VEN: OutputPin,
    DELAY: DelayNs,
{
    /// Wrap already-configured handles without touching the link (the
    /// zero-knowledge constructor; prefer [`bring_up`](Self::bring_up),
    /// which powers the controller and proves it answers).
    pub fn new(i2c: I2C, irq: IRQ, ven: VEN, delay: DELAY, addr: u8) -> Self {
        Self {
            i2c,
            irq,
            ven,
            delay,
            addr,
            ntf: Deque::new(),
        }
    }

    /// Constructor + bring-up self-test over pure embedded-hal
    /// (t_4864e74e): the VEN power cycle, then the zero-length probe
    /// IMMEDIATELY after VEN rise — the PN7160 only ACKs within a
    /// window after the rise (bench-proven 2026-10-08, same window rule
    /// as the esp transport's `bringup_config_order`). `Ok` = the
    /// controller is powered and answering on `addr`.
    pub fn bring_up(
        i2c: I2C,
        irq: IRQ,
        ven: VEN,
        delay: DELAY,
        addr: u8,
    ) -> Result<Self, HalLinkError> {
        let mut t = Self::new(i2c, irq, ven, delay, addr);
        t.ven_cycle();
        if t.link_probe() {
            Ok(t)
        } else {
            Err(HalLinkError::NoAckAfterVenCycle)
        }
    }

    /// VEN power cycle, same ordering as the esp transport (assert,
    /// hold, drop, off, raise, settle) — public so the init-ladder
    /// retry path can re-apply it between attempts.
    pub fn ven_cycle(&mut self) {
        let _ = self.ven.set_high();
        self.delay.delay_ms(VEN_HOLD_MS);
        let _ = self.ven.set_low();
        self.delay.delay_ms(VEN_OFF_MS);
        let _ = self.ven.set_high();
        self.delay.delay_ms(VEN_SETTLE_MS);
    }

    /// Stateless link liveness probe: the zero-length I2C write (the
    /// embedded-hal shape of the esp `i2c_master_probe`). Safe while a
    /// session is active — never a state-machine-touching transact.
    pub fn link_probe(&mut self) -> bool {
        self.i2c.write(self.addr, &[]).is_ok()
    }

    fn irq_raised(&mut self) -> bool {
        self.irq.is_high().unwrap_or(false)
    }

    /// Two-phase NCI read (nxp-nci style, same as the esp transport):
    /// 3-byte header, then payload.
    fn read_frame(&mut self) -> Option<Frame> {
        let mut hdr = [0u8; 3];
        if self.i2c.read(self.addr, &mut hdr).is_err() {
            return None;
        }
        let plen = hdr[2] as usize;
        let mut pkt = [0u8; 3 + 255];
        pkt[..3].copy_from_slice(&hdr);
        if plen > 0 && self.i2c.read(self.addr, &mut pkt[3..3 + plen]).is_err() {
            return None;
        }
        Frame::decode(&pkt[..3 + plen])
    }

    fn push_ntf(&mut self, f: Frame) {
        if self.ntf.is_full() {
            let _ = self.ntf.pop_front();
        }
        let _ = self.ntf.push_back(f);
    }

    fn pop_ntf(&mut self) -> Option<Frame> {
        self.ntf.pop_front()
    }
}

impl<I2C, IRQ, VEN, DELAY> Transport for HalPn7160Transport<I2C, IRQ, VEN, DELAY>
where
    I2C: I2c,
    IRQ: InputPin,
    VEN: OutputPin,
    DELAY: DelayNs,
{
    fn send(&mut self, cmd: &[u8]) -> bool {
        self.i2c.write(self.addr, cmd).is_ok()
    }

    fn poll_frame(&mut self) -> Option<Frame> {
        if self.irq_raised() {
            self.read_frame()
        } else {
            None
        }
    }

    fn transact(&mut self, cmd: &[u8]) -> Option<Frame> {
        self.i2c.write(self.addr, cmd).ok()?;
        let mut waited = 0u32;
        loop {
            if self.irq_raised() {
                let f = self.read_frame()?;
                // ONE shared reply-routing decision with the esp
                // transport and the host mock (is_transact_reply): the
                // reply to what we sent is the first NON-NTF frame;
                // NTFs surfaced mid-transact are stashed for drain().
                if is_transact_reply(&f) {
                    return Some(f);
                }
                self.push_ntf(f);
                continue;
            }
            if waited >= IRQ_WAIT_BUDGET_MS {
                return None;
            }
            self.delay.delay_ms(1);
            waited += 1;
        }
    }

    fn drain(&mut self) -> Option<Frame> {
        if let Some(f) = self.pop_ntf() {
            return Some(f);
        }
        if self.irq_raised() {
            self.read_frame()
        } else {
            None
        }
    }

    fn wait_ms(&mut self, ms: u64) {
        self.delay.delay_ms(ms as u32);
    }

    fn link_probe(&mut self) -> bool {
        // Same probe primitive as the esp transport's override: the
        // inherent method, not a default `false`.
        HalPn7160Transport::link_probe(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::Pn7160Driver;
    use crate::{
        GID_CORE, GID_RF, MT_NTF, MT_RSP, OID_CORE_INIT, OID_CORE_RESET, OID_RF_DISCOVER,
        OID_RF_DISCOVER_MAP, STATUS_OK,
    };
    use std::collections::VecDeque;

    // ---- mock embedded-hal pieces (t_4864e74e: the constructor is
    // ---- host-testable over pure traits — no esp anything).

    #[derive(Debug)]
    enum MockI2cErr {
        Nak,
    }

    #[derive(Default)]
    struct MockI2c {
        /// Addresses that ACK a (zero-length) probe write.
        alive: Vec<u8>,
        /// Scripted RX byte stream, drained by every `read`.
        rx: VecDeque<u8>,
        /// Recorded writes: (addr, payload). Zero-length = probes.
        writes: Vec<(u8, Vec<u8>)>,
    }

    impl embedded_hal::i2c::Error for MockI2cErr {
        fn kind(&self) -> embedded_hal::i2c::ErrorKind {
            embedded_hal::i2c::ErrorKind::NoAcknowledge(
                embedded_hal::i2c::NoAcknowledgeSource::Address,
            )
        }
    }

    impl embedded_hal::i2c::ErrorType for MockI2c {
        type Error = MockI2cErr;
    }

    impl I2c for MockI2c {
        fn transaction(
            &mut self,
            addr: u8,
            operations: &mut [embedded_hal::i2c::Operation<'_>],
        ) -> Result<(), Self::Error> {
            use embedded_hal::i2c::Operation;
            for op in operations {
                match op {
                    Operation::Write(buf) => {
                        self.writes.push((addr, buf.to_vec()));
                        if !self.alive.contains(&addr) {
                            return Err(MockI2cErr::Nak);
                        }
                    }
                    Operation::Read(buffer) => {
                        if !self.alive.contains(&addr) {
                            return Err(MockI2cErr::Nak);
                        }
                        for b in buffer.iter_mut() {
                            *b = self.rx.pop_front().unwrap_or(0);
                        }
                    }
                }
            }
            Ok(())
        }
    }

    struct MockIrq(core::cell::Cell<bool>);

    impl embedded_hal::digital::ErrorType for MockIrq {
        type Error = core::convert::Infallible;
    }

    impl InputPin for MockIrq {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            Ok(self.0.get())
        }
        fn is_low(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.0.get())
        }
    }

    #[derive(Default)]
    struct MockVen {
        /// Level transitions in call order (1=high, 0=low).
        log: std::vec::Vec<u8>,
    }

    impl embedded_hal::digital::ErrorType for MockVen {
        type Error = core::convert::Infallible;
    }

    impl OutputPin for MockVen {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.log.push(0);
            Ok(())
        }
        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.log.push(1);
            Ok(())
        }
    }

    #[derive(Default)]
    struct MockDelay {
        ms: core::cell::Cell<u32>,
    }

    impl DelayNs for MockDelay {
        fn delay_ns(&mut self, ns: u32) {
            self.ms.set(self.ms.get() + ns / 1_000_000);
        }
    }

    fn mocks() -> (MockI2c, MockIrq, MockVen, MockDelay) {
        (
            MockI2c {
                alive: vec![0x28],
                ..MockI2c::default()
            },
            MockIrq(core::cell::Cell::new(false)),
            MockVen::default(),
            MockDelay::default(),
        )
    }

    /// Script one NCI frame on the wire: payload bytes queue up and the
    /// IRQ line raises (the NFCC raises IRQ when it has data).
    fn script_frame(i2c: &mut MockI2c, irq: &MockIrq, bytes: &[u8]) {
        for b in bytes {
            i2c.rx.push_back(*b);
        }
        irq.0.set(true);
    }

    #[test]
    fn bring_up_cycles_ven_and_probes_inside_the_window() {
        let (i2c, irq, ven, delay) = mocks();
        let ms_before = delay.ms.get();
        let t = HalPn7160Transport::bring_up(i2c, irq, ven, delay, 0x28)
            .expect("alive address must ACK");
        // VEN cycle ordering: high (hold), low (off), high (settle).
        assert_eq!(t.ven.log, vec![1, 0, 1]);
        // Timings actually waited: 10 + 50 + 50 ms.
        assert_eq!(t.delay.ms.get() - ms_before, 110);
        // The probe is the LAST bus action, right after VEN rise —
        // the ACK-window rule — and is a zero-length write.
        let (addr, payload) = t.i2c.writes.last().unwrap();
        assert_eq!(*addr, 0x28);
        assert!(payload.is_empty());
    }

    #[test]
    fn bring_up_fails_loudly_on_a_dead_link() {
        let (mut i2c, irq, ven, delay) = mocks();
        i2c.alive.clear();
        assert_eq!(
            HalPn7160Transport::bring_up(i2c, irq, ven, delay, 0x28).err(),
            Some(HalLinkError::NoAckAfterVenCycle)
        );
    }

    #[test]
    fn transport_link_probe_is_the_zero_length_write() {
        let (i2c, irq, ven, delay) = mocks();
        let mut t = HalPn7160Transport::new(i2c, irq, ven, delay, 0x28);
        assert!(Transport::link_probe(&mut t));
        assert_eq!(t.i2c.writes, vec![(0x28, vec![])]);
    }

    #[test]
    fn send_writes_the_command_to_the_controller_address() {
        let (i2c, irq, ven, delay) = mocks();
        let mut t = HalPn7160Transport::new(i2c, irq, ven, delay, 0x28);
        assert!(t.send(&[0x20, 0x00, 0x00]));
        assert_eq!(t.i2c.writes, vec![(0x28, vec![0x20, 0x00, 0x00])]);
    }

    #[test]
    fn poll_frame_is_irq_gated() {
        let (mut i2c, irq, ven, delay) = mocks();
        script_frame(&mut i2c, &irq, &[0x40, 0x00, 0x01, 0x00]);
        let mut t = HalPn7160Transport::new(i2c, irq, ven, delay, 0x28);
        // IRQ low: never touches the bus, the scripted bytes stay queued.
        t.irq.0.set(false);
        assert!(t.poll_frame().is_none());
        assert_eq!(t.i2c.rx.len(), 4);
        // IRQ raised: the frame decodes.
        t.irq.0.set(true);
        let f = t.poll_frame().expect("IRQ raised must read");
        assert_eq!((f.mt, f.gid, f.oid, f.len), (0x40, 0x00, 0x00, 1));
    }

    #[test]
    fn transact_routes_the_first_non_ntf_and_stashes_the_ntf() {
        let (mut i2c, irq, ven, delay) = mocks();
        // An NTF lands before the RSP on the wire.
        script_frame(&mut i2c, &irq, &[MT_NTF, OID_CORE_RESET, 0x01, 0x00]);
        script_frame(
            &mut i2c,
            &irq,
            &[MT_RSP | GID_CORE, OID_CORE_RESET, 0x01, STATUS_OK],
        );
        let mut t = HalPn7160Transport::new(i2c, irq, ven, delay, 0x28);
        let f = t
            .transact(&[0x20, 0x00, 0x00])
            .expect("RSP must surface despite the leading NTF");
        assert_eq!(f.mt, MT_RSP);
        // The NTF was stashed, not dropped — drain() hands it over.
        let ntf = t.drain().expect("stashed NTF must drain");
        assert_eq!((ntf.mt, ntf.oid), (MT_NTF, OID_CORE_RESET));
    }

    #[test]
    fn transact_times_out_on_a_silent_controller() {
        let (i2c, irq, ven, delay) = mocks();
        let mut t = HalPn7160Transport::new(i2c, irq, ven, delay, 0x28);
        // IRQ never raises, no data scripted — the budget (200 ticks)
        // is honored and the caller gets None, not a hang.
        assert!(t.transact(&[0x20, 0x00, 0x00]).is_none());
        assert!(t.delay.ms.get() >= 200);
    }

    /// The whole construction chain over PURE embedded-hal, host-run:
    /// handles → bring-up (VEN + window probe) → the crate's init
    /// ladder over the NCI wire → a live driver. This is the
    /// compile-and-behave proof for the t_4864e74e seam: no board
    /// table, no esp binding, no firmware main involved.
    #[test]
    fn crate_ladder_bring_up_runs_over_the_pure_hal_transport() {
        let (mut i2c, irq, ven, delay) = mocks();
        // The NCI bring-up ladder's replies, in wire order (same
        // script as the crate driver's MockTransport ladder test).
        script_frame(
            &mut i2c,
            &irq,
            &[MT_RSP | GID_CORE, OID_CORE_RESET, 0x01, STATUS_OK],
        );
        script_frame(&mut i2c, &irq, &[MT_NTF, OID_CORE_RESET, 0x01, 0x00]);
        script_frame(
            &mut i2c,
            &irq,
            &[MT_RSP | GID_CORE, OID_CORE_INIT, 0x01, STATUS_OK],
        );
        script_frame(
            &mut i2c,
            &irq,
            &[MT_RSP | GID_RF, OID_RF_DISCOVER_MAP, 0x01, STATUS_OK],
        );
        script_frame(
            &mut i2c,
            &irq,
            &[MT_RSP | GID_RF, OID_RF_DISCOVER, 0x01, STATUS_OK],
        );

        let t = HalPn7160Transport::bring_up(i2c, irq, ven, delay, 0x28)
            .expect("controller answers the window probe");
        let mut driver = Pn7160Driver::new(t);
        driver.init().expect("NCI ladder over the pure HAL link");
    }
}
