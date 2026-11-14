// SPDX-FileCopyrightText: © 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Generic ISO14443-4 block protocol handler.
//!
//! Manages block numbering, block construction (with optional CID), and
//! chain accumulation. Returns [`Action`]s that the caller (PCD or PICC
//! transport layer) must execute.
// ---------------------------------------------------------------------------
// QUOTE-ONLY COPY (never compiled, never linked) — trimmed excerpt.
// See reference/iso14443-rs/PROVENANCE.md for upstream pin + license.
// Original path in the upstream tree: src/type_a/protocol.rs
// ---------------------------------------------------------------------------

    /// Validate an S(WTX) request and count it.
    ///
    /// §7.3: the INF field is one byte whose low six bits code a WTXM of 1
    /// to 59; 0 and 60 to 63 are RFU. A peer that keeps asking for more time
    /// without ever answering is stopped by the WTX counters.
    fn check_wtx(&mut self, block: &Block) -> Result<(), TypeAError> {
        let wtxm = match block.payload.as_slice() {
            [inf] => inf & 0x3f,
            _ => return Err(self.exceeded(Limit::WtxValue)),
        };
        if !(1..=59).contains(&wtxm) {
            return Err(self.exceeded(Limit::WtxValue));
        }

        self.counters.consecutive_wtx = self.counters.consecutive_wtx.saturating_add(1);
        if self.counters.consecutive_wtx > self.limits.max_consecutive_wtx {
            return Err(self.exceeded(Limit::ConsecutiveWtx));
        }

        self.counters.total_wtx = self.counters.total_wtx.saturating_add(1);
        if self.counters.total_wtx > self.limits.max_total_wtx {
            return Err(self.exceeded(Limit::TotalWtx));
        }

        Ok(())
    }
