# Reference-First Audit Findings (2026-10-11, branch `repo-audit`)

Full-project quality audit per the AGENTS.md Reference-First Doctrine:
wire-format constants and logic were checked for (a) a traceable
reference citation (spec section, tracked text source, or verbatim
quote marker covered by `specquotes.toml`), and (b) a unit test that
locks the bytes. DRY, CI, and docs hygiene were swept alongside.

**Verification method**: every item below was verified against the
tracked reference sources in `reference/` (read from the initialized
submodules) plus upstream pcsc-lite/ccid sources for host-side wire
truth. Nothing in this table relies on memory or on issue text.

**Severity**: P1 = reference-verified wire deviation (wrong bytes
emitted/parsed); P2 = missing citation or duplication that invites a
future wire bug; P3 = hygiene/documentation debt.

## Findings

| # | Sev | Location | Gap | Reference evidence | Status |
|---|-----|----------|-----|--------------------|--------|
| 1 | P1 | `crates/ccid-core/src/secure.rs` `PinVerifyParams::parse` | `wPINMaxExtraDigit` parsed as `min=data[4], max=data[5]` — **swapped**. Hosts build the u16 as `(min << 8) + max` (pcsc-lite `scardcontrol.c`: `wPINMaxExtraDigit = (PIN_min_size << 8) + PIN_max_size;`), and libccid copies the struct little-endian verbatim into the CCID payload (`commands.c` SecurePINVerify: "At this point we now have the above 3 variables in little endian" + the verbatim copy loop). LE bytes are therefore `[max, min]`. The local doc table ("Max (high), Min (low)") describes the byte order backwards; `reader.h` says "0xXXYY where XX is minimum … YY is maximum" — i.e. **min in the HIGH byte of the u16 = second byte on the wire**. | `reference/CCID/MacOSX/reader.h` (PIN_VERIFY_STRUCTURE), `reference/CCID/src/commands.c` (SecurePINVerify), pcsc-lite `examples/scardcontrol.c` | **Fixed** (this branch) + reference-locked tests |
| 2 | P1 | `crates/ccid-core/src/secure.rs` `PinVerifyParams::parse` | APDU template read from offset 12. The §6.1.11 tail is `bTeoPrologue[3]` (three bytes — `reader.h`: `uint8_t bTeoPrologue[3];`), so the host-side APDU lands at payload offset **14**. Our 2-byte-short model made `apdu_template = [prologue[1], prologue[2], CLA, INS]` for real pcscd traffic. | same as #1 | **Fixed** + reference-locked tests |
| 3 | P1 | `crates/ccid-core/src/secure.rs` `PinModifyParams::parse` | §6.1.12 parse (a) omits `bConfirmPIN` (reads `bEntryValidationCondition` from the confirm byte), (b) shifts wLangId/bMsgIndex1 by one, (c) ignores that libccid **omits `bMsgIndex2` when `bNumberMessage == 0` and `bMsgIndex3` when `bNumberMessage < 3`** (`commands.c` SecurePINModify loop comments: "in CCID the bMsgIndex2 is present only if bNumberMessage != 0"), (d) reads the APDU at 16 instead of 17–19 depending on that variant. | `reference/CCID/MacOSX/reader.h` (PIN_MODIFY_STRUCTURE), `reference/CCID/src/commands.c` (SecurePINModify) | **Fixed** (variant-aware parse) + tests for all three forms |
| 4 | P1 | `crates/ccid-protocol/src/atr.rs` `FI_TABLE[0] = 0` | ISO 7816-3:2006 Table 7 row 0000 is Fi=372 (and is the default Fd) — osmo's `iso7816_3_fi_table` (annotated `@implements ISO/IEC 7816-3:2006(E) table 7`) is `[372, 372, 558, 744, …]`. Ours had `0` at index 0, and the `unwrap_or(372)` fallback never fires because `get(0)` is `Some(0)` — `fi_from_ta1_high(0)` returned 0 (nonsense ETU math downstream). | `reference/osmo-ccid-firmware/ccid_common/iso7816_3.c` | **Fixed** + full-table lock test |
| 5 | P2 | `crates/ccid-protocol/src/atr.rs` | FI/DI tables, `AtrParams::default` (Fi=372/Di=1, IFSC=32, BWI=4, CWI=13), and `classify_t0_procedure_byte` carried **no citations at all** (0 quote markers in the file). | ISO 7816-3:2006 §8.3 (defaults), Table 7/8, §10.3.3 (T=0 procedure bytes) via osmo `iso7816_3.{c,h}` | **Fixed**: `ISO7816:` quote markers + new `specquotes.toml` source |
| 6 | P2 | `crates/ccid-core/src/handler.rs:390` | Post-dispatch error accounting uses raw `response[7] & 0x40` — osmo's `CCID_CMD_STATUS_FAILED` is the pre-shifted `0x40` mask value; our crate constant `COMMAND_STATUS_FAILED = 0x01` (unshifted) made the quoted comment and the constant disagree. | `ccid_proto.h` (`#define CCID_CMD_STATUS_FAILED 0x40`, `CCID_CMD_STATUS_MASK 0xC0`) | **Fixed**: bit derived from the crate constant + comment |
| 7 | P2 | `crates/ccid-core/src/handler.rs` contact XfrBlock error shapes | Literals `0xFE` (inactive) / `0xFF` (card failure) / `0x07` (oversize) instead of the quoted `CCID_ERR_ICC_MUTE` / `CCID_ERR_CMD_ABORTED` constants (byte-identical values — mechanical). | `ccid_proto.h` Table 6.2-2 enum | **Fixed** (0xFE/0xFF only; 0x07 is a `bError` with no ccid_proto.h constant — annotated as a literal with a comment) |
| 8 | P2 | `firmware/ccid-firmware/src/ccid.rs` | `CLOCK_FREQUENCY_KHZ = 4000` / `DATA_RATE_BPS = 10752` hardcoded for **all** profiles — duplicating `device_profile.rs` values and **wrong for CT30/K30** (4800 kHz / 12903 bps per their reference files). The class-specific control responses advertise Cherry values on Gemalto builds. | `reference/CCID/readers/Gemalto_IDBridge_{CT30,K30}.txt`, `device_profile.rs` | **Fixed**: derived from `CURRENT_PROFILE` |
| 9 | P2 | `specquotes.toml` coverage | `secure.rs`, `atr.rs`, `diagnostics.rs` had zero quote markers; the PIN structures' host-side truth (pcsc-lite `reader.h`) and the libccid PCSC→CCID translation (`commands.c`) were not trackable sources at all. | — | **Fixed**: sources `PCSC` (reader.h), `LIBCCID` (commands.c), `ISO7816` (osmo iso7816_3.c) added; markers written and `greatspectations check` run locally |
| 10 | P2 | `.github/workflows/release.yml` + `Dockerfile` | Release pipeline references **nonexistent features** (`profile-cherry-st2100`, `profile-gemalto-plain`, `profile-gemalto-pinpad` — none exist in `firmware/ccid-firmware/Cargo.toml`); a tag push would fail at cargo configure. Release notes also still claim K30 = `08e6:3437` **with PIN pad** — the exact false claim corrected in CHANGELOG Unreleased (real K30: 3438, `bPINSupport=0x00`). | `firmware/ccid-firmware/Cargo.toml` `[features]`, AGENTS.md profile table | **Fixed** |
| 11 | P2 | `docs/CCID_SPEC_AUDIT.md` §GetParameters | Quoted implementation snippet is stale: `p.bwi.wrapping_sub(1).min(0x0A)` as `bWaitingIntegersT1` — current code packs `(bwi << 4) | (cwi & 0x0F)` (BWI/CWI nibbles per Table 6.2-3). Doc no longer matches the tree. | current `handler.rs parameter_payload` | **Fixed** (doc annotated with the current packing) |
| 12 | P3 | `crates/ccid-protocol/src/types.rs` `COMMAND_STATUS_*` | Constants are unshifted 2-bit values (0/1/2) while the adjacent quote block shows osmo's pre-shifted 0x00/0x40/0x80 — a convention trap for direct `bStatus` comparisons. `build_bstatus` handles it correctly; only documentation was ambiguous. | `ccid_proto.h` | **Fixed** (comment clarifies pre-shift semantics; existing `build_bstatus` tests already lock the encoding) |
| 13 | P3 | `firmware/ccid-firmware/src/protocol_unit.rs` | File duplicates the `ccid-protocol` crate's own tests verbatim (same assertions, "Inspired by" comments) — double maintenance, and the copies modeled the same gaps (see #4). | — | **Fixed**: test duplication dropped, re-export surface kept |
| 14 | P3 | `firmware/ccid-firmware/src/ccid_core.rs` | `PIN_PAD_VENDOR_ID = 0x046A` and the Gemalto `0x6A` firmware-features escape array carry no citation. | `reference/CCID/readers/Cherry SmartTerminal ST2xxx.txt`, `Gemalto IDBridge CT30.txt` | **Fixed** (citations added in comments) |
| 15 | P3 | `firmware/ccid-firmware/src/ccid_core.rs` `notify_slot_change_bytes` | USB §6.3.1 bit layout (bit0 = card present, bit1 = changed) built from raw `0x01`/`0x02` without the citation the serial twin has. | CCID Rev 1.1 §6.3.1 | **Fixed** (comment; semantics cross-checked against `ccid-transport-serial` CARD_PRESENT=0x03/CARD_ABSENT=0x02) |
| 16 | P3 | `crates/ccid-core/src/handler.rs` `ICC_NOT_ACTIVE = 0x05` | Documented as a GemPC Twin serial vendor convention (explicitly *not* a Table 6.2-2 code) but no tracked citation exists for it; bench-pinned by the conformance battery. | bench (issue #92 fuzz/conformance gate) | **Noted** — left as-is: documented vendor behavior, byte already locked by `test_xfr_block_when_not_powered_returns_icc_not_active` |
| 17 | P3 | `crates/ccid-protocol/src/{types,atr}.rs` tests | `Xorshift32` PRNG helper duplicated in two test modules. | — | **Fixed** (shared `#[cfg(test)]` helper in `ccid-protocol`) |
| 18 | P3 | `DEFAULT_T0_PARAMS` / `DEFAULT_T1_PARAMS` | Bytes carry a spec-section citation (Table 6.2-3) but no *tracked text source* exists to verbatim-quote (osmo has no default byte arrays; the CCID spec PDF is not in-tree as text). Tests are self-referential. | CCID Rev 1.1 Table 6.2-3 | **Noted** — doctrine-acceptable (spec-section citation); test comments upgraded to name the exact fields each byte encodes |

## Areas checked and found clean

- `crates/ccid-transport-serial/src/lib.rs`: SYNC/CTRL/LRC framing fully
  quote-marked from `ccid_serial.c` and byte-locked by tests
  (`[0x03, 0x15, 0x16]` NAK golden, boundary/fuzz suite). No gaps.
- `crates/ccid-core/src/diagnostics.rs`: 28-byte Escape 0xD0 layout has a
  golden byte-exact test and a layout table. No gaps.
- `build_bstatus` encoding (bits [7:6] cmd / [1:0] ICC): verified against
  `ccid_proto.h` `CCID_ICC_STATUS_MASK 0x03` / `CCID_CMD_STATUS_MASK 0xC0`.
- `.github/workflows/ci.yml`: matrix, cache-generation discipline, and
  path filters match AGENTS.md (verified section by section).
- STM32 `ccid_core.rs` wrapper vs `ccid-core` handler: the split is the
  documented DRY phase-2 shape; no uncontrolled duplication found beyond
  items #7/#8.

## Blast-radius notes for the P1 fixes

The PIN-parse corrections (#1–#3) change `PinVerifyParams` /
`PinModifyParams` field values for pcscd-shaped input. Consumers:

- ESP32 firmware: does **not** reference the PIN params (verified by
  grep) — untouched by this change.
- STM32 pinpad UI (`pinpad/state.rs`): consumes `min_len`/`max_len`
  only as struct fields — semantics now match what pcscd actually sent.
- The labgrid HIL pinpad test only checks feature advertisement
  (`VERIFY MODIFY` via `getFeatureRequest`), so no bench-pinned wire
  behavior is contradicted; the previous offsets were never exercised
  against real pcscd PIN traffic. Bench verification of a full
  `SCardControl(FEATURE_VERIFY_PIN_DIRECT)` round-trip remains the
  recommended follow-up (mock-only changes don't count until then).

## Docs / CI hygiene findings (second sweep, docs-agent leads verified in-tree)

| # | Sev | Location | Gap | Status |
|---|-----|----------|-----|--------|
| 19 | P2 | `docs/ESP32_CCID_SPEC_AUDIT.md` | "open investigation: #89" — #89 is CLOSED (confirmed battery timing false positive via #107 forensics); "#88 positive case pending" — #88 is CLOSED/resolved (auto-activation model, bench-verified 2026-10-10) | **Fixed** (claims annotated with the resolutions) |
| 20 | P2 | `docs/ESP32_CCID_SPEC_AUDIT.md` | dead path `tests/test_ccid.py` (file exists nowhere in the tree) | **Fixed** (annotated historical; current equivalent named) |
| 21 | P2 | `docs/MILESTONES.md` | "Next steps" section listed issues #10/#9/#8/#6/#7 — all CLOSED (the crates/ restructuring landed); 8 broken `../src/*` links from the pre-workspace flat layout | **Fixed** (past-tense + links rewritten to `../firmware/ccid-firmware/src/*`, all targets verified to exist; missing historical root docs noted in a header) |
| 22 | P3 | `docs/MILESTONES-AUDIT.md` | "No `CHANGELOG.md`" — CHANGELOG.md exists at repo root; `profile-gemalto-pinpad` referenced as a live feature | **Fixed** (annotated as snapshot-era) |
| 23 | P2 | `.github/workflows/release.yml` `reproducibility-report` job | Still echoed the dead profile names after the main matrix fix | **Fixed** |
| 24 | P3 | `docs/CODEX_PR_FEEDBACK_TRIAGE.md` | References `.omo/pr-triage.md` (untracked scratch dir — absent in fresh checkouts) | **Noted** — cross-session scratch pointer, harmless |
| 25 | P3 | `docs/nucula-campaign/{RUNBOOK,README}.md` | Closed-issue references framed as active | **Noted** — both files are declared "Archived as-is … campaign log, not maintained documentation" (historical by design) |
| 26 | P3 | `docs/{DESIGN_DECISIONS,CCID_SPEC_AUDIT_REPORT,LIBCCID_DRIVER_AUDIT_REPORT,C_REFERENCE_AUDIT_REPORT}.md` | Bare `src/...` paths from the pre-workspace layout | **Noted** — all targets resolve under `firmware/ccid-firmware/src/` (verified); `readers/supported_readers.txt` in LIBCCID_DRIVER_AUDIT_REPORT is a libccid-source-layout reference, not a repo file |
| 27 | P3 | `README.md` | Repository-guide bullet claimed `protocol_unit.rs` carries its own `#[cfg(test)]` module (dropped as a duplicate of the crate suites) | **Fixed** |

Verified-clean in the same sweep: `docs/SPECIFICATIONS.md` reader-spec
references (valid once `reference/CCID` submodule is initialized — note
the submodule is NOT initialized in fresh worktrees by default, which
makes those paths *look* missing locally), CODEX triage table (all 53
rows ADDRESSED, consistent with closed #113/#114), and
PERF_AND_DRY_STRATEGY's open-issue citations (#87/#92/#121 all genuinely
open).
