//! DWT (Data Watchpoint and Trace) cycle counter watchdog.
//!
//! Provides accurate wall-clock timeouts on Cortex-M3+ cores using the DWT
//! `CYCCNT` register — a free-running 32-bit cycle counter. Pattern proven in
//! `gm65-scanner` (commit 1d7fddc) and already partially used in the F746
//! bitbang driver; this module makes it reusable for the F469 USART path and
//! any future timing-sensitive code.
//!
//! # Range
//!
//! CYCCNT is `u32`. At 180 MHz (F469), it wraps every ~23.8 s. All
//! `wrapping_sub` math in this module is monotonic over a single
//! timeout window, so wraps are handled correctly as long as the
//! caller does not construct timeouts longer than ~23 s.

#![cfg_attr(all(target_arch = "arm", target_os = "none"), allow(dead_code))]

pub const fn ms_to_cycles(ms: u32, hz: u32) -> u32 {
    let cycles = (ms as u64).saturating_mul(hz as u64) / 1000;
    if cycles > u32::MAX as u64 {
        u32::MAX
    } else {
        cycles as u32
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DwtWatchdog {
    start: u32,
    timeout: u32,
}

impl Default for DwtWatchdog {
    fn default() -> Self {
        Self::new(0)
    }
}

impl DwtWatchdog {
    pub const fn new(timeout_cycles: u32) -> Self {
        Self {
            start: 0,
            timeout: timeout_cycles,
        }
    }

    pub const fn from_ms(ms: u32, hz: u32) -> Self {
        Self::new(ms_to_cycles(ms, hz))
    }

    pub fn start(&mut self) {
        self.start = unsafe { cyccnt() };
    }

    pub fn expired(&self) -> bool {
        unsafe { cyccnt().wrapping_sub(self.start) >= self.timeout }
    }

    pub fn elapsed_cycles(&self) -> u32 {
        unsafe { cyccnt().wrapping_sub(self.start) }
    }

    pub fn remaining_cycles(&self) -> u32 {
        unsafe { self.timeout.saturating_sub(cyccnt().wrapping_sub(self.start)) }
    }

    pub const fn timeout_cycles(&self) -> u32 {
        self.timeout
    }

    pub const fn start_cycles(&self) -> u32 {
        self.start
    }

    #[cfg(test)]
    pub fn set_start_for_test(&mut self, start: u32) {
        self.start = start;
    }

    #[cfg(test)]
    pub fn with_start_for_test(timeout_cycles: u32, start: u32) -> Self {
        Self {
            start,
            timeout: timeout_cycles,
        }
    }
}

#[cfg(all(target_arch = "arm", target_os = "none"))]
mod arm {
    const DWT: *mut u32 = 0xE000_1000usize as *mut u32;
    const DWT_CTRL_W: usize = 0x000 / 4;
    const DWT_CYCCNT_W: usize = 0x004 / 4;
    const DEMCR: *mut u32 = 0xE000_EDFCusize as *mut u32;
    const DEMCR_TRCENA: u32 = 1 << 24;
    const DWT_CTRL_CYCCNTENA: u32 = 1 << 0;

    /// Initialize DWT CYCCNT. Idempotent. Call once at startup before any
    /// [`super::DwtWatchdog`] use.
    #[inline]
    pub unsafe fn init() {
        core::ptr::write_volatile(DEMCR, core::ptr::read_volatile(DEMCR) | DEMCR_TRCENA);
        core::ptr::write_volatile(DWT.add(DWT_CYCCNT_W), 0);
        core::ptr::write_volatile(
            DWT.add(DWT_CTRL_W),
            core::ptr::read_volatile(DWT.add(DWT_CTRL_W)) | DWT_CTRL_CYCCNTENA,
        );
    }

    #[inline(always)]
    pub unsafe fn cyccnt() -> u32 {
        core::ptr::read_volatile(DWT.add(DWT_CYCCNT_W))
    }
}

#[cfg(all(target_arch = "arm", target_os = "none"))]
pub use arm::*;

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
pub fn init() {}

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
mod host {
    use core::sync::atomic::{AtomicU32, Ordering};

    pub static HOST_CYCCNT: AtomicU32 = AtomicU32::new(0);

    #[inline(always)]
    pub fn cyccnt() -> u32 {
        HOST_CYCCNT.load(Ordering::Relaxed)
    }

    #[inline(always)]
    pub fn set_cyccnt(value: u32) {
        HOST_CYCCNT.store(value, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn advance_cyccnt(by: u32) {
        HOST_CYCCNT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.wrapping_add(by)))
            .ok();
    }

    #[inline(always)]
    pub fn reset_cyccnt() {
        HOST_CYCCNT.store(0, Ordering::Relaxed);
    }
}

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
pub use host::cyccnt;

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
pub fn set_host_cyccnt_for_test(value: u32) {
    host::set_cyccnt(value);
}

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
pub fn advance_host_cyccnt_for_test(by: u32) {
    host::advance_cyccnt(by);
}

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
pub fn reset_host_cyccnt_for_test() {
    host::reset_cyccnt();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_test_clock() {
        reset_host_cyccnt_for_test();
    }

    #[test]
    fn ms_to_cycles_basic_conversions() {
        assert_eq!(ms_to_cycles(1000, 1_000_000), 1_000_000);
        assert_eq!(ms_to_cycles(100, 168_000_000), 16_800_000);
        assert_eq!(ms_to_cycles(1, 180_000_000), 180_000);
    }

    #[test]
    fn ms_to_cycles_zero_ms_is_zero_cycles() {
        assert_eq!(ms_to_cycles(0, 1_000_000_000), 0);
    }

    #[test]
    fn ms_to_cycles_zero_hz_is_zero_cycles() {
        assert_eq!(ms_to_cycles(100, 0), 0);
    }

    #[test]
    fn ms_to_cycles_saturates_on_overflow() {
        let v = ms_to_cycles(u32::MAX, 1_000_000_000);
        assert_eq!(v, u32::MAX);
    }

    #[test]
    fn from_ms_equivalent_to_new_plus_ms_to_cycles() {
        let a = DwtWatchdog::from_ms(200, 168_000_000);
        let b = DwtWatchdog::new(ms_to_cycles(200, 168_000_000));
        assert_eq!(a.timeout_cycles(), b.timeout_cycles());
        assert_eq!(a.timeout_cycles(), 33_600_000);
    }

    #[test]
    fn new_defaults_start_to_zero() {
        reset_test_clock();
        let wd = DwtWatchdog::new(1000);
        assert_eq!(wd.start_cycles(), 0);
        assert_eq!(wd.timeout_cycles(), 1000);
    }

    #[test]
    fn default_has_zero_timeout() {
        let wd = DwtWatchdog::default();
        assert_eq!(wd.timeout_cycles(), 0);
    }

    #[test]
    fn expired_is_true_for_zero_timeout_watchdog() {
        reset_test_clock();
        let mut wd = DwtWatchdog::new(0);
        wd.start();
        assert!(wd.expired());
    }

    #[test]
    fn expired_false_before_threshold_true_after() {
        reset_test_clock();
        let mut wd = DwtWatchdog::new(1000);
        wd.start();
        assert!(!wd.expired());

        advance_host_cyccnt_for_test(999);
        assert!(!wd.expired());

        advance_host_cyccnt_for_test(1);
        assert!(wd.expired());

        advance_host_cyccnt_for_test(1000);
        assert!(wd.expired());
    }

    #[test]
    fn elapsed_cycles_tracks_advance() {
        reset_test_clock();
        let mut wd = DwtWatchdog::new(1_000_000);
        wd.start();
        assert_eq!(wd.elapsed_cycles(), 0);

        advance_host_cyccnt_for_test(500);
        assert_eq!(wd.elapsed_cycles(), 500);

        advance_host_cyccnt_for_test(250);
        assert_eq!(wd.elapsed_cycles(), 750);
    }

    #[test]
    fn remaining_cycles_saturates_to_zero() {
        reset_test_clock();
        let mut wd = DwtWatchdog::new(1000);
        wd.start();

        assert_eq!(wd.remaining_cycles(), 1000);

        advance_host_cyccnt_for_test(400);
        assert_eq!(wd.remaining_cycles(), 600);

        advance_host_cyccnt_for_test(10_000);
        assert_eq!(wd.remaining_cycles(), 0);
    }

    #[test]
    fn start_resets_window() {
        reset_test_clock();
        let mut wd = DwtWatchdog::new(1000);
        wd.start();
        advance_host_cyccnt_for_test(900);
        assert!(!wd.expired());

        wd.start();
        assert!(!wd.expired());
        advance_host_cyccnt_for_test(900);
        assert!(!wd.expired());

        advance_host_cyccnt_for_test(100);
        assert!(wd.expired());
    }

    #[test]
    fn wrapping_subtraction_handles_cyccnt_rollover() {
        let start = u32::MAX - 100;
        let wd = DwtWatchdog::with_start_for_test(500, start);
        set_host_cyccnt_for_test(start);
        assert!(!wd.expired());

        set_host_cyccnt_for_test(u32::MAX);
        assert!(!wd.expired());

        set_host_cyccnt_for_test(400);
        assert!(wd.expired());
    }

    #[test]
    fn init_is_callable_on_host_and_is_noop() {
        init();
        init();
    }
}
