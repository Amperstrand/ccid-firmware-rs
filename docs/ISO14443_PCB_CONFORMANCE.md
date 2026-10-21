# ISO 14443-4 PCB semantics: standard vs. pinned implementation — the #372 analysis

Status: verified 2026-10-21 · Scope: the `iso14443` crate's PCD session as
pinned by `firmware/esp32-ccid` (Amperstrand/iso14443-rs) · Evidence:
hardware A/B on the bench (m5stick + MFRC522 + J3R180), host-side
wire-level mock (`firmware/esp32-ccid/src/pcd_sustained_load_tests.rs`),
source diff of both pins.

This document records a standards-vs-implementation discrepancy
investigation the way we did for T=1 contact PCBs in #330 (ETSI TS 102
221 Table 7.5, "b5 RFU in I-blocks"). The trigger was issue #372:
sustained GlobalPlatform LOAD on the m5stick/MFRC522 path failing with
SW 6400 after a dependency re-pin, while single APDUs stayed green.

## 1. What the standard says (ISO/IEC 14443-4 PCB coding)

Established wire facts used throughout (consistent with the pinned
crate's encoder and its own tests, and with every ISO-DEP capture we
have on file):

| Block | PCB base | Carries | Notes |
|---|---|---|---|
| I-block | `0x02` | N(S) on **b1**; chaining on **b5** (`0x10`); NAD b4; CID b3 | b2=1 in all blocks |
| R(ACK)  | `0xA2` | N(R) on b1 | b5=0 ⇒ ACK, b5=1 ⇒ NAK |
| S(WTX req) | `0xF2` | 1-byte WTXM | b5..b4=11 marks WTX |
| S(WTX resp) | `0xF2` | echoes WTXM | req/resp not distinguished in the byte |

Rules that matter here (§7.5.4 chaining, §7.5.7 WTX, block-number
rules; the rewritten crate cites them as "Rule 6/7/11/13/B/E"):

1. The sender toggles its block number after each I-block **sent**.
2. An R(ACK) from the receiver acknowledges the last I-block; the
   number it carries identifies what the receiver expects next.
3. S(WTX) is the PICC's legal mechanism to extend the frame waiting
   time — repeatedly if needed (each request multiplies FWT by WTXM).
   The standard does not cap how often a card may ask.

## 2. What each pin implements

### Old pin (`268db79`, pre-2026-10-21)

- **PCD-side chaining never toggles N(S)**: every chained I-block goes
  out with the same block number, and an R(ACK) is accepted only when
  it **echoes** that number (`process_rblock` compares against the
  untoggled counter). A strictly rule-1-conformant card answering with
  the toggled number would put this session into an infinite
  retransmit loop.
- **No work budgets at all**: WTX runs, retransmissions and frame
  counts are unbounded (the recovery path retries forever).
- ATS parsing does not verify CRC_A.

### New pin (`73db6d4`, current)

- Toggling per rule 1, echo-style R(ACK) acceptance per rule 2,
  CRC-verified ATS, FSD enforcement on incoming frames, and per-exchange
  work limits (defaults: `max_consecutive_wtx: 8`, `max_total_wtx: 32`,
  `max_retransmissions: 3`, `max_frames: 512`).
- The protocol logic is *more* conformant than the old pin. The
  budgets are an anti-DoS policy choice, not a standards requirement —
  see §4.

## 3. The failure, isolated (both options tested)

Hardware A/B (same board, card, CAP, command; images differ only in
the dependency set):

| Image | Result |
|---|---|
| current main, new pins | `gp --install` → LOAD failed **SW 6400 ×3**; single APDUs, fuzz 87/0, soak 1000/1000 green |
| main code + old pins (268db79/dccf86e3) | `gp --install` **GREEN** (after stray-package cleanup) |
| pre-re-pin image (e9173657-era) | `gp --install` **GREEN** |

Host-side mechanism probe (wire-level mock, no hardware): a 460-byte
chained APDU — the gp LOAD shape — whose response phase contains **20
consecutive S(WTX)** — the flash-writing-ISD shape — against the new
pin:

- Default limits → `Err(Protocol(LimitExceeded(ConsecutiveWtx)))`.
- Limits raised (`consecutive 64 / total 4096 / frames 4096 / retrans 5`)
  → exchange completes, 460/460 INF bytes delivered exactly once,
  response reassembled in order, all 20 WTX granted.

Both probes are committed as regression tests
(`pcd_sustained_load_tests.rs`).

## 4. Is it a bug? The argument, both ways

**For "the new pin is correct"**: every protocol-level change we
audited matches the cited rules better than the old pin. The old pin's
constant-N(S) chain is a genuine §7.5.4 deviation that real cards
tolerate; the new pin's toggling is what the standard prescribes. The
budgets themselves are defensible: an unbounded WTX/retransmit loop is
a liveness hazard for a reader (the old pin could hang forever on a
malfunctioning card).

**For "the new pin regressed us"**: the standard permits a card to ask
for waiting time repeatedly; capping *consecutive* WTX at 8 with no
consumer override in the defaults means any PCD/card combination whose
legitimate steady state includes long WTX runs — exactly a
flash-writing secure element during package install, at
MFRC522-over-I2C speeds — dies. The defaults encode an assumption
(fast reader, quick card) that our hardware violates by design. On our
bench this was indistinguishable from a broken reader.

**Verdict (final, 2026-10-21 evening — supersedes the limits-era analysis below):**
the wire capture settled it: the deployed J3R180 population **rejects
the spec-correct S(WTX response) encoding** (0xF3) — after a 0xF3
grant the card goes silent (three grants, zero answers; netlog capture
in #378). The fork therefore sends the request-form echo (0xF2 + WTXM,
fork PR #13) — a deliberate, documented, reversible deviation (the b1
model stays for a strict-mode opt-in). With 0xF2 grants the FULL
conformance set installs GREEN on hardware. Limits-era verdict kept for
history: not a protocol-conformance bug in the new pin; a **policy
default that does not fit slow-PCCD + flash-writing-card workflows**,
plus an old pin that was silently non-conformant and worked only
because real cards are lenient about PCD-side block numbers. The
correct place to fix it is the consumer: `PcdSession::set_limits` is
public, so the driver now configures explicit budgets sized for this
PCD class instead of forking or re-pinning.

## 5. What we shipped

- Keep the new pins (`iso14443@73db6d4`, `mfrc522-pcd@2a1afea`).
- `mfrc522_driver.rs`: `set_limits(consecutive 64, total 4096, frames
  4096, retransmissions 5)` at session construction, with the #372
  rationale inline.
- Regression tests pinning the wire contract (chaining integrity,
  echo-R(ACK), WTX dance) **and** the budget semantics (defaults abort
  a 20-run; configured limits survive it). Any future re-pin that
  changes either must consciously update these tests.
- This document.

## 6. Follow-ups (update 2026-10-21: items 2–3 DONE)

1. OPEN (owner-filed): upstream note to Foundation-Devices re: default
   `max_consecutive_wtx` for slow PCDs / a "patient reader" preset.
2. DONE — fork PR #12 (fa4f91f8), failing-test-first: S-block b1
   modeled as the response indicator; WTX grants now always 0xF3,
   state-independent (previously the I/R block-number counter leaked
   onto b1). Old-pin constant-BN deviation is historical (that line is
   superseded).
3. DONE — fork PR #12: mid-chain S(WTX) granted per §7.5.7 (chain
   continues, bounded by Limits). Not observed on the J3R180 wire, so
   the failing wire-mock is the evidence; upstreamable as-is.
4. NEW: prepare the Foundation-Devices upstream PR carrying both fixes
   (tests included). Blocked on owner filing per the member-org rule.

## 6-old. Follow-ups filed upstream (owner-filed per member-org rule)

1. Ask upstream to document/soften the default `max_consecutive_wtx`
   (8) for slow-PCD use cases, or expose a "patient reader" preset.
2. Report the old pin's non-toggling PCD chain as a historical
   conformance note (it worked against lenient cards; worth recording
   for anyone bisecting similar regressions).
3. Consider handling S(WTX) mid-PCD-chain (currently `Other` in both
   pins; the standard permits the PICC to answer a chained I-block
   with S(WTX)). No evidence the J3R180 does this — filed as a
   robustness gap, not a defect we hit.

## 7. Precedent

Same investigation class as #330/#345 on the T=1 contact wire (ETSI
TS 102 221 Table 7.5): implementation strictness vs. real-hardware
leniency deciding the outcome. Lesson recorded: when a dependency
re-pin touches protocol state machines, run the sustained-exchange
harness — not just single-APDU smokes — before calling it green.
