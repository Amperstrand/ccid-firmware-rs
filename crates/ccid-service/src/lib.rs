//! Board-agnostic CCID serving kernel (reader mode).
//!
//! Extraction home (t_72e1e2ed) for the serving machinery that used to
//! live inside `firmware/esp32-ccid`: the GemPC-Twin serial server
//! (`ccid_serial_server`), the vpcd transport (`vpcd`), the SPP pump
//! (`spp_server`), and the CCID handler / T=1 endpoint / driver contract
//! they share.
//!
//! # Boundary
//!
//! This crate depends only on `card-interface`, `ccid-core`,
//! `ccid-protocol` and `ccid-transport-serial` (+ std/heapless). It
//! cannot see board internals — pins, peripherals, frontends are the
//! firmware bin's job ("board table + frontend constructor + service
//! spawn"). Any board-specific need crosses this boundary as data:
//! injected drivers (`nfc::NfcDriver`), installed hooks (`t1::install_clock`,
//! the handler's card-presence sink), or byte transports.
//!
//! # Mode seam
//!
//! Reader-mode serving lives here; the standalone-mode (bolty-core)
//! firmware is a separate consumer of the same kernel. Modes stay
//! explicit so a future merged firmware selects them by feature flag or
//! runtime config — never by `cfg` inside this crate.

pub mod ccid_handler;
pub mod ccid_types;
pub mod frontend;
pub mod nfc;
pub mod t1;
pub mod turnaround;
