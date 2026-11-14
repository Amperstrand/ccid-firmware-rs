# reference/iso14443-rs/ — quote-only excerpts (PROVENANCE)

Upstream: [github.com/Amperstrand/iso14443-rs](https://github.com/Amperstrand/iso14443-rs)
— this repo's canonical ISO 14443 fork (same source bolty-rs uses),
rebased on the Foundation Devices main line.

Pin: rev `0f7d9d70` (merge of PR #13 `wtx-grant-interop`, 2026-10) —
the EXACT rev `firmware/esp32-ccid/Cargo.toml` builds against. (Note:
AGENTS.md still cites the older `73db6d4` re-pin; the manifest is the
authority. `73db6d4` carried the same `check_wtx` WTXM 1..59 lock.)

License: GPL-3.0-or-later (SPDX headers kept verbatim in the excerpt;
© 2025 Foundation Devices, Inc.). Quote-only — never compiled, never
linked (class-1 reference copy per the AGENTS.md vendoring policy).

## Files

| File | Original path | What we quote |
|---|---|---|
| `protocol-wtx.rs` | `src/type_a/protocol.rs` (trimmed to `check_wtx`) | ISO 14443-4 §7.3 WTXM 1..59 range check — the NFC-side (ISO-DEP) cross-cite for the T=1 WTX multiplier's 59 ceiling (`crates/ccid-service/src/t1.rs`, #142 G-04) |

## Why a quote-only copy

The crate is consumed as a canonical git dep (never build-vendored,
issues #19/#6). The corpus needs a pinned, local text so
`specquotes.toml` can drift-check quotes (`[sources.ISODEP]`, marker
`ISO14443`). On a dep re-pin, re-copy the excerpt and update this file
in the same commit — the spec-quote-drift CI leg then re-verifies
every quote against the new revision.
