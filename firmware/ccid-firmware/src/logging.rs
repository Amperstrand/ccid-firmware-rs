#[cfg(all(target_arch = "arm", target_os = "none"))]
macro_rules! ccid_info {
    ($($arg:tt)*) => { defmt::info!($($arg)*) };
}

#[cfg(all(target_arch = "arm", target_os = "none"))]
macro_rules! ccid_warn {
    ($($arg:tt)*) => { defmt::warn!($($arg)*) };
}

#[cfg(all(target_arch = "arm", target_os = "none"))]
macro_rules! ccid_error {
    ($($arg:tt)*) => { defmt::error!($($arg)*) };
}

#[cfg(all(target_arch = "arm", target_os = "none"))]
macro_rules! ccid_debug {
    ($($arg:tt)*) => { defmt::debug!($($arg)*) };
}

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

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
macro_rules! ccid_error {
    ($($arg:tt)*) => { log::error!($($arg)*) };
}

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
macro_rules! ccid_debug {
    ($($arg:tt)*) => { log::debug!($($arg)*) };
}

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
macro_rules! ccid_trace {
    ($($arg:tt)*) => { log::trace!($($arg)*) };
}
