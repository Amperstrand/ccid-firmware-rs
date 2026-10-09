//! `log` crate sink that queues records for the BLE debug console
//! (issue #66). `log()` only enqueues — never blocks, never touches the
//! radio — and the main loop pumps the queue into GATT notifications at
//! safe points via `drain()`. Queue semantics live in the host-tested
//! `ble_log_queue` module.

#[cfg(all(any(target_arch = "xtensa", target_arch = "riscv32"), feature = "ble"))]
mod imp {
    use std::sync::{Mutex, OnceLock};

    use crate::ble_debug::BleDebugServer;
    use crate::ble_log_queue::{format_record, LogQueue};

    struct LoggerState {
        queue: LogQueue,
        subscribers_attached: bool,
    }

    pub struct BleLogger {
        state: Mutex<LoggerState>,
    }

    impl BleLogger {
        pub fn new() -> Self {
            Self {
                state: Mutex::new(LoggerState {
                    queue: LogQueue::new(),
                    subscribers_attached: false,
                }),
            }
        }

        pub fn global() -> &'static Self {
            static LOGGER: OnceLock<BleLogger> = OnceLock::new();
            LOGGER.get_or_init(Self::new)
        }

        /// Installs this logger as the crate-global `log` sink. Must run
        /// BEFORE any `EspLogger::initialize_default()` — the log crate
        /// accepts exactly one global logger, and the ESP console logger
        /// claims it permanently (first installer wins).
        pub fn install() -> Result<&'static Self, log::SetLoggerError> {
            let logger = Self::global();
            log::set_logger(logger)?;
            Ok(logger)
        }

        fn lock_state(&self) -> std::sync::MutexGuard<'_, LoggerState> {
            self.state.lock().unwrap_or_else(|e| {
                log::warn!("BLE logger lock poisoned — recovering");
                e.into_inner()
            })
        }

        /// Pump queued lines into the GATT server. Called at safe points
        /// in the main loop (post-command, read-idle); a no-op while no
        /// central subscribes — the queue keeps filling (drop-oldest) so
        /// a late-attaching central still gets recent history behind the
        /// "attached; N lines dropped" banner.
        pub fn drain(&self, server: &BleDebugServer) {
            if !server.has_subscribers() {
                self.lock_state().subscribers_attached = false;
                return;
            }

            {
                let mut state = self.lock_state();
                if !state.subscribers_attached {
                    state.subscribers_attached = true;
                    let banner = state.queue.take_banner();
                    drop(state);
                    let _ = server.send_log_bytes(&banner);
                }
            }

            loop {
                let next = {
                    let state = self.lock_state();
                    state.queue.front().map(|line| line.to_vec())
                };

                let Some(next) = next else {
                    break;
                };

                if server.send_log_bytes(&next) {
                    self.lock_state().queue.pop_front();
                } else {
                    // Delivery latched off mid-drain: the failed line stays
                    // at the front for the next attach/drain.
                    break;
                }
            }
        }

        fn enqueue(&self, line: Vec<u8>) {
            self.lock_state().queue.push_line(line);
        }
    }

    impl log::Log for BleLogger {
        fn enabled(&self, _metadata: &log::Metadata) -> bool {
            true
        }

        fn log(&self, record: &log::Record) {
            if !self.enabled(record.metadata()) {
                return;
            }

            let module = record.module_path().unwrap_or(record.target());
            let message = record.args().to_string();
            let line = format_record(&record.level().to_string(), module, &message);
            self.enqueue(line);
        }

        fn flush(&self) {}
    }
}

#[cfg(not(all(any(target_arch = "xtensa", target_arch = "riscv32"), feature = "ble")))]
mod imp {
    use crate::ble_debug::BleDebugServer;

    #[derive(Default)]
    pub struct BleLogger;

    impl BleLogger {
        pub fn new() -> Self {
            Self
        }

        pub fn global() -> &'static Self {
            static LOGGER: BleLogger = BleLogger;
            &LOGGER
        }

        pub fn install() -> Result<&'static Self, log::SetLoggerError> {
            Ok(Self::global())
        }

        pub fn drain(&self, _server: &BleDebugServer) {}
    }
}

pub use imp::*;
