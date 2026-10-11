#[cfg(all(target_arch = "arm", target_os = "none"))]
macro_rules! ccid_info {
    ($($arg:tt)*) => { defmt::info!($($arg)*) };
}

#[cfg(all(target_arch = "arm", target_os = "none"))]
macro_rules! ccid_warn {
    ($($arg:tt)*) => { defmt::warn!($($arg)*) };
}

// ccid_error!/ccid_trace! are part of the logging vocabulary kept for
// wrapper/debug paths; allow unused so host builds stay warning-clean
// when a port moves call sites into the shared core (which logs through
// the `log` facade instead).
#[allow(unused_macros)]
#[cfg(all(target_arch = "arm", target_os = "none"))]
macro_rules! ccid_error {
    ($($arg:tt)*) => { defmt::error!($($arg)*) };
}

#[cfg(all(target_arch = "arm", target_os = "none"))]
macro_rules! ccid_debug {
    ($($arg:tt)*) => { defmt::debug!($($arg)*) };
}

#[allow(unused_macros)]
#[cfg(all(target_arch = "arm", target_os = "none"))]
macro_rules! ccid_trace {
    ($($arg:tt)*) => { defmt::trace!($($arg)*) };
}

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
macro_rules! ccid_info {
    ($($arg:tt)*) => { log::info!($($arg)*) };
}

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
macro_rules! ccid_warn {
    ($($arg:tt)*) => { log::warn!($($arg)*) };
}

#[allow(unused_macros)]
#[cfg(not(all(target_arch = "arm", target_os = "none")))]
macro_rules! ccid_error {
    ($($arg:tt)*) => { log::error!($($arg)*) };
}

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
macro_rules! ccid_debug {
    ($($arg:tt)*) => { log::debug!($($arg)*) };
}

#[allow(unused_macros)]
#[cfg(not(all(target_arch = "arm", target_os = "none")))]
macro_rules! ccid_trace {
    ($($arg:tt)*) => { log::trace!($($arg)*) };
}
