//! Board tables as const data — Layer A of the boards-as-data design
//! (`docs/BOARDS-AS-DATA-DESIGN.md`, kanban t_099ed2af pilot).
//!
//! Pure, `no_std`, core-only const data: raw GPIO numbers, enums, bools —
//! the same interface shape today's firmware already uses (e.g.
//! `let (sda_gpio_no, scl_gpio_no) = (32, 33);` feeding
//! `pn7160_nci::hal_i2c::I2cMasterBus::new(port, sda, scl)`). esp-idf-hal
//! type-state tokens are a Layer-B constructor's job (the firmware bins),
//! never a table's — no `const` struct can hand out `Gpio13<'static>`.
//!
//! Precedent: `crates/pn7160-nci/src/board.rs` ("pure data — host-testable,
//! no esp deps"). This crate generalizes that block to the whole board.
//!
//! Boundary (design §3.2): consumed by firmware bins, tooling, and CI —
//! NEVER by `ccid-service` (the #383 board-blind serving kernel). Adding a
//! `boards` dep to ccid-service fails `tools/union_matrix.py check` — that
//! tripwire is intended.
//!
//! No runtime board auto-detection by design: board selection stays a
//! compile-time Cargo feature; a feature selects a table entry instead of
//! carrying pin data inline.
#![cfg_attr(not(test), no_std)]

pub mod m5atom;
pub mod m5stick;
pub mod nucula_gen;
pub mod schema;
pub mod stm32f469;
pub mod stm32f746;

pub use schema::*;

pub use m5atom::M5ATOM;
pub use m5stick::M5STICK;
pub use nucula_gen::NUCULA;
pub use stm32f469::F469;
pub use stm32f746::F746;

/// Every registered board (design §5: registry ↔ module files ↔ bin
/// features cross-checked by `tools/union_matrix.py check`'s boards leg).
/// A board's substantive content is one module `src/<board>.rs`; the
/// registry line + `pub mod` are the two mechanical one-liners the design
/// admits (§4 "adding a board touches one file"). nucula's module is
/// GENERATED (§2.1) — regenerate with
/// `cargo run -p boards-gen -- --board nucula`.
pub const ALL_BOARDS: &[&BoardDesc] = &[&M5STICK, &M5ATOM, &NUCULA, &F469, &F746];
