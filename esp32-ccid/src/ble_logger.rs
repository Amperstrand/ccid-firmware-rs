#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
mod imp {
    use std::collections::VecDeque;
    use std::fmt::Write as _;
    use std::sync::{Mutex, OnceLock};

    use crate::ble_debug::BleDebugServer;

    pub const MAX_LOG_LINE_LEN: usize = 200;
    const QUEUE_CAPACITY: usize = 32;

    #[derive(Default)]
    struct LoggerState {
        queue: VecDeque<Vec<u8>>,
    }

    pub struct BleLogger {
        state: Mutex<LoggerState>,
    }

    impl BleLogger {
        pub fn new() -> Self {
            Self {
                state: Mutex::new(LoggerState::default()),
            }
        }

        pub fn global() -> &'static Self {
            static LOGGER: OnceLock<BleLogger> = OnceLock::new();
            LOGGER.get_or_init(Self::new)
        }

        pub fn install() -> Result<&'static Self, log::SetLoggerError> {
            let logger = Self::global();
            log::set_logger(logger)?;
            Ok(logger)
        }

        pub fn drain(&self, server: &BleDebugServer) {
            if !server.has_subscribers() {
                return;
            }

            loop {
                let next = {
                    let state = self.state.lock().unwrap();
                    state.queue.front().cloned()
                };

                let Some(next) = next else {
                    break;
                };

                if server.send_log_bytes(&next) {
                    self.state.lock().unwrap().queue.pop_front();
                } else {
                    break;
                }
            }
        }

        fn enqueue(&self, line: Vec<u8>) {
            let mut state = self.state.lock().unwrap();

            if state.queue.len() >= QUEUE_CAPACITY {
                state.queue.pop_front();
            }

            state.queue.push_back(line);
        }

        fn format_record(record: &log::Record) -> Vec<u8> {
            let module = record.module_path().unwrap_or(record.target());
            let mut rendered = String::new();
            let _ = write!(
                &mut rendered,
                "[{}] {}: {}\n",
                record.level(),
                module,
                record.args()
            );

            let mut bytes = rendered.into_bytes();
            if bytes.len() > MAX_LOG_LINE_LEN {
                bytes.truncate(MAX_LOG_LINE_LEN.saturating_sub(1));
                if bytes.last().copied() != Some(b'\n') {
                    bytes.push(b'\n');
                }
            }

            bytes
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

            self.enqueue(Self::format_record(record));
        }

        fn flush(&self) {}
    }
}

#[cfg(not(all(target_arch = "xtensa", feature = "backend-mfrc522")))]
mod imp {
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

        pub fn drain(&self, _server: &()) {}
    }
}

pub use imp::*;
