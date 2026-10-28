# SPEC-SOURCES — free authoritative proxies for paywalled ISO/IEC specs

Policy doc (issue #140). The rule, the ladder, and what is already wired
in this tree. Companion to the Reference-First Doctrine in `AGENTS.md`
and the provenance index `reference/README.md`.

## The rule

> **Never cite a paywalled ISO/IEC clause (7816-*, 14443-*) in code,
> tests, docs or issue comments without either (a) a tracked proxy quote
> (verbatim, with the proxy's own section/table cite) or (b) a named
> reference-implementation line (file:line in a tracked submodule or
> canonical fork).**

"ISO 7816-3 §11.6.2 says BWT = 11 + 2^BWI etu" written from memory is
the founding incident class of this repo (the PN7160 TLV weeks, the
K30 PIN-pad claim) — an un-cited numeric claim is a bug, not a fact.
The paywalled PDFs may not be vendored or transcribed wholesale (see
`AGENTS.md` → Vendoring policy); the proxies below restate the same
wire facts freely and quotably.

## The ladder

For each paywalled domain: the FREE authoritative proxy, verified URL,
and what it is authoritative FOR. Availability last verified 2026-10-28.

### ISO/IEC 7816-3 (electricals, timing, T=0/T=1 character+block layers)

| Proxy | Where | Authority for |
|---|---|---|
| **EMV Contact Book 1 — Application Independent ICC to Terminal Interface Requirements** (EMV 4.3, free "Public Access" download) | https://www.emvco.com/emv-technologies/contact/ | The classic public restatement of 7816-3: ETU/Fi/Di, speed classes (TA1), guard time, character frame+parity, WT tables (CWT/BWT/BGT), T=0 procedure bytes, T=1 block layer. EMVCo states the spec basis is ISO/IEC 7816 (FAQ on the same page). Already indexed: `docs/SPECIFICATIONS.md` § "Additional Resources → EMV Contact Interface Specification". |
| osmo-ccid-firmware `ccid_common/iso7816_3.c` (tracked submodule @ 639db14, GPL-2.0+) | `reference/osmo-ccid-firmware/ccid_common/iso7816_3.c` | Fi/Di/fmax tables with `@implements ISO 7816-3:2006` annotations — the ATR-parsing authority in-tree (specquotes `ISO7816`). |
| libccid's openct `proto-t1.c` (tracked submodule @ 1a0f75d) | `reference/CCID/src/openct/proto-t1.c` | The T=1 engine the HOST actually runs: PCB bits, block build/receive rules, retries (specquotes `T1`; triangulated in `docs/T1_BEST_PRACTICES.md`). |

### ISO/IEC 7816-4 (APDU encoding, secure channel, card lifecycle)

| Proxy | Where | Authority for |
|---|---|---|
| **GlobalPlatform Card Specification v2.2.1 / v2.3** (free PDFs from GP) | https://www.globalplatform.org/specifications/ (Specs → Secure Element) | The public restatement of the 7816-4 command/APDU subset that our cards actually run: SELECT/GET DATA shapes, status-word conventions, and the full GP secure-channel layer (INITIALIZE UPDATE, EXTERNAL AUTHENTICATE, SCP02/03) that the #127/#132/#143 analysis leans on. GP APDU tables carry their own clause numbering — cite GP's, never an ISO clause number from memory. |
| osmo-ccid-firmware + pcsc-lite (tracked) | `reference/osmo-ccid-firmware/`, `reference/pcsc-lite/` | Host-side card-lifecycle contracts (power-on/off, protocol selection, PPS policy — `src/prothandler.c`). |

### UICC behaviors (waiting-time conventions as deployed by SIMs)

| Proxy | Where | Authority for |
|---|---|---|
| **ETSI TS 102 221** (UICC-terminal interface; free public delivery) | https://www.etsi.org/deliver/etsi_ts/102200_102299/102221/ (e.g. `17.04.00_60/ts_102221v170400p.pdf`) | 7816-3/-4 semantics as millions of SIMs deploy them: ATR/PPS specifics, waiting-time conventions, status-word handling. Quote ONLY where a bench card actually exercises the behavior (P71/JCOP class) — no spec-tourism (issue #140 rule). |

### ISO/IEC 14443 (contactless PICC)

| Proxy | Where | Authority for |
|---|---|---|
| **Linux kernel NFC stack** (GPL-2.0, quotable) | https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/net/nfc + `include/net/nfc/nci.h`, `include/uapi/` | NCI core, param IDs, ISO-DEP/14443 framing constants (already doctrine — `docs/nucula-campaign/nfc_citation_map.md`). |
| NXP PN7160 UM11495 (free PDF; verbatim quotes tracked) | `docs/quote-sources/um11495-quotes.md` (specquotes `UM11495`) | NCI chip-specific config, TOTAL_DURATION — the free authority the TLV incident locked. |
| `Amperstrand/iso14443-rs` fork @ `ai-experiments` (canonical git dep) | `firmware/esp32-ccid/Cargo.toml` | ISO 14443-3/4 bit- and frame-level restatement in code, same source bolty-rs uses. |

### CCID Rev 1.1 (NOT paywalled — free PDF, quote freely)

| Source | Where | Authority for |
|---|---|---|
| USB CCID Rev 1.1 spec PDF (USB-IF document library, free) | `docs/SPECIFICATIONS.md` → "USB.org Document Library" link | The wire spec itself; audit trail in `docs/CCID_SPEC_AUDIT.md` with `// CCID_SPEC:` markers (osmo `ccid_proto.h` is the tracked TEXT form — specquotes `CCID_SPEC`). |

### PC/SC host contract (free)

| Source | Where | Authority for |
|---|---|---|
| PC/SC specifications | https://pcscworkgroup.com/specs/ (v2 part 10 PIN/FEATURE tables) | Host API + FEATURE_* semantics. |
| pcsc-lite + libccid (tracked submodule / quote-only copy) | `reference/pcsc-lite/`, `reference/CCID/` | What pcscd ACTUALLY does — the authority for host-contract claims (`docs/PCSC_HOST_CONTRACT_AUDIT.md`). |

## What is already wired (do not duplicate)

- **specquotes.toml** (enforced by `.github/workflows/spec-quote-drift.yml`):
  `CCID_SPEC` (osmo ccid_proto.h), `CCID_SERIAL`/`LIBCCID`/`GEMPC`/`PCSC`/
  `CCID_DEFS`/`IFDHANDLER` (reference/CCID), `ISO7816` (osmo iso7816_3.c),
  `T1` (openct proto-t1.c), `OSMO_FSM` (ccid_slot_fsm.c), `PCSCD`
  (pcsc-lite), `UM11495` + `M5AXP` (docs/quote-sources/ PDF transcriptions).
- **docs/quote-sources/**: verbatim-quote markdown for PDF-only authorities
  (the specquotes approach for sources with no text form).
- **reference/README.md**: provenance index of every tracked source
  (upstream pin, license, what we quote).
- **docs/T1_BEST_PRACTICES.md**: the openct/ST-cited T=1 triangulation.

## Workflow: adding a new proxy quote

1. Verify the proxy is free and authoritative (this ladder or a new row
   justified in the PR); record the URL + access date.
2. Transcribe the quote VERBATIM into `docs/quote-sources/<name>-quotes.md`
   with the proxy's own section/table/page cite — never an ISO clause
   number unless the proxy itself prints one.
3. Wire `[sources.<MARKER>]` in `specquotes.toml` pointing at the
   markdown (or the tracked source file), quote it in code with
   `// <MARKER>: ...` and lock with a unit test (GREAT EXPECTATION
   annotation) — if the test breaks, re-verify against the citation,
   never "fix" the test to match new unverified bytes.
4. Add the source to `reference/README.md` if it is a new upstream.

## Known gaps (tracked in #140, not done here)

- EMV Book 1 timing tables (CWT/BWT/BGT/guard time) not yet extracted
  into `docs/quote-sources/emv-book1-timing.md` for the timing lane.
- `docs/T1_BEST_PRACTICES.md` tables are triangulated but not yet
  promoted to specquotes-locked quote blocks.
- The tree-wide grep audit for un-backed ISO 7816/14443 numeric claims
  (source-or-flag pass) has not been run.
