# HIL Verification Protocol — card registry, test manifests, human-ops queue

> **Status: draft.** This document generalizes the verification-recipe shape
> established by PR #321 (the protocol pilot) into a standing protocol. It
> adds **no CI gates and changes no behavior** — it defines three artifacts
> (a card registry, a per-claim test-manifest format, a human-ops queue) so
> that every hardware/bench claim in this repo is reproducible, card-safe,
> and traceable to evidence. Prior-art search before authoring (2026-10-21):
> no `HIL-VERIFICATION-PROTOCOL*` or equivalent exists in-repo; the adjacent
> artifacts are `tests/hardware/nfc/J3R180-RUNBOOK.md` (lifecycle envelope),
> PR #321's merge-gating recipe, `docs/NFC-FIELD-LOCKS-DESIGN.md` (PR #354,
> draft), the ACR1252 field recorder (PR #355, in flight), and
> `tools/hil/README.md` (bench runner runbook). This doc references all of
> them and duplicates none.

## 0. Why

PR #321 set the shape: a firmware fix whose **merge was gated on a numbered
hardware-verification recipe** — build/flash with proven boot, expected
on-the-wire behavior, a functional card leg with the card-safety rules
inlined ("pcscd SELECT + CPLC only, NO GP auth per #127 ledger rules"),
a regression guard, and "post evidence here". That shape worked, but it
lives in a PR body. The recurring failure modes this protocol exists to
kill:

- **Claims without a card identity.** "Verified on the bench card" is
  meaningless when cards physically move between coils and slots (they do —
  see §3). The runbook's results table carries a row that is still
  "pending physical card move" (F469), and PR #327's swapped-card
  experiment proved the card, not the reader, was the variable in a
  week-long debugging arc.
- **Card-safety rules re-derived per session.** The #127 ISD-lock incident
  (AGENTS.md "Card-Handling Doctrine") happened because a hammer treated
  SC-class commands as free. Every new harness must inherit the safe/unsafe
  table, not rediscover it.
- **Evidence that is prose, not data.** Phase lines, JSONL events, and
  escape counters exist on the bench today; PR descriptions re-narrate them
  instead of binding to them.
- **Physical operations with no protocol.** Card moves and cable replugs
  happen off-tool by whoever is nearest; the agent has no way to request
  them or verify they happened except by inferring from side effects.

Rule inherited from the Reference-First Doctrine (AGENTS.md): every
bench-result claim below cites a real in-repo source (path, issue, or PR);
known unknowns are marked as such, never papered over.

## 1. Card registry — the J3R180 dev JavaCards

### 1.1 Identity is CPLC, never ATR

The bench dev cards are NXP **P71D321 silicon, JCOP4 — "J3R180" is NXP's
JCOP4 R-series product name on that silicon** (AGENTS.md Open-gaps §4;
issue #127: "All silicon: NXP P71D321 (JCOP4, J3R180 family, fab batch
6782)").

ATR is **negotiated per reader and per interface from the ATS** and
therefore cannot identify a card: the *same* card answers
`3B 85 80 01 80 …` through the ACR1252 and `3B 85 81 31 FE 45 …` through
the m5stick MFRC522 (`tests/hardware/nfc/J3R180-RUNBOOK.md`, preflight
notes). The ATR *family* prefixes are a useful wrong-card guard (the
lifecycle tool's preflight #4) but prove family, not identity.

The **CPLC** (Card Production Life Cycle), read via plain
`GET DATA 80 CA 9F7F 00` — a pure read, unlimited per the AGENTS.md
safe/unsafe table — is the identity key:

| CPLC field (offset) | Example value | Meaning (bench cards) |
|---|---|---|
| IC fabricator (0–1) | `4790` | NXP (issue #327: identical on all compared cards) |
| IC type (2–3) | `D321` | P71D321 silicon = J3R180 |
| OS id / release (4–7) | `4700 0000 0000` | ROM/OS level — the field #327 wants compared next |
| IC fabrication date (8–9) | `3129` | fab date |
| **IC serial number (10–13)** | `50972320` | **registry key (with batch)** |
| IC batch (14–15) | `6782` | fab batch — `6782` on the whole #127 set |

Field layout cross-check: issue #155 records a capture in exactly this
shape — `4790D321 … 3129 5090 6420 6782`.

### 1.2 Registry rows

Key = `serial/batch`. **Seat (which coil/slot a card sits on) is volatile
state, not identity** — cards move by design (§3, HOP-1); the seat column
records only the last cited sighting. Permanent co-location as an
*invariant* is the lane of `docs/NFC-FIELD-LOCKS-DESIGN.md` (PR #354,
`co-location.json`, CPLC-keyed) — this registry supplies the identity layer
that design builds on.

| Key | Pre-pers | GP / ISD state | Last cited seat | Sources |
|---|---|---|---|---|
| `50972320/6782` | `164B` | **M5stick lifecycle pilot card** (§4). Full-CPLC capture on record starts `4790d321 4700 0000 0000 3129 50972320 6782` (trailing module/personalization fields to be captured whole at next read — see #327). #127-era history: 3 auth attempts burned under the org-wide key hunt; that budget is spent, do not repeat it. | m5stick coil | #127 table; runbook §Results (2026-10-20); #327 comment (2026-10-20 swap experiment) |
| `50947320/6782` | `1619` | **ISD LOCKED — permanent fixture.** Answers `69FF` to all SC-class commands, survives power-off; no recovery path exists (#144). Applets on it remain usable: applet fixture only, no GP install/delete. | ACR1252 coil (SmartPGP-era) | AGENTS.md Card-Handling Doctrine; #127; #144 |
| `50942720/6782` | `1647` | Working AUTH triple (org dev keyset) — the keyed card the 2026-10-12 m5stick CAP-install plan wanted moved onto the coil. | was on ACR1252 coil | AGENTS.md m5stick row; #132; #127 |
| `50861320/6782` | `1633` | Unknown pre-personalized keys — read-only relay test card. | m5stick coil (2026-10-12) | #127 |
| `50906420/6782` | n/a (post-#127 acquisition) | Unknown — **not in the #127 ledger**; default-ISD SELECT answers `6A82`; do NOT run gp against it (speculative auth). CPLC on record: `4790D321 … 3129 5090 6420 6782`. | m5stick coil (2026-10-13) | #155; AGENTS.md m5stick row |
| *(serial pending CPLC capture)* | — | **Ex-SmartPGP-provisioned card** — shipped SmartPGP v1.23.2.0 *preloaded*; the only card fully functional for applet data ops through native-USB readers. Serial + full CPLC capture is queued (#327 "full CPLC comparison"). | nucula coil | runbook §Results FINDING; #327 |

Per issue #127's conclusion, the pre-`1647`/`164B`/`1633` cards with
unknown pre-personalized keys are **read-only relay test cards** unless the
owner obtains their keys; GP lifecycle work runs on keyed cards. The
2026-10-20 runbook-era cards authenticate with the **org dev keyset**
(single-sourced in `tests/hardware/nfc/J3R180-RUNBOOK.md` §Keys and
`tests/soak/soak_02_globalplatform.py` history).

**Open registry work** (tracked, not blocked): full-CPLC captures for every
row (#327), serial for the nucula-coil card, and the co-location invariant
(PR #354). A row's "unknown" is data, not shame — write it down.

### 1.3 Card-state invariants

- **INV-C1 — baseline-or-applet.** A managed J3R180 is always in exactly
  one of two states: its **resting baseline** or **baseline + the OpenPGP
  applet under test**. The lifecycle tool enforces this by construction:
  cleanup runs *before* baseline capture (so the captured state is the true
  resting state, not baseline+strays), and exit 0 requires
  `baseline registry restored` to equal the captured registry
  (`tests/hardware/nfc/j3r180_gpg_lifecycle.py`, `baseline_pkgs()` +
  final phase). The observed resting baseline is **4 packages** (runbook
  §Results: "baseline ✅ 4 pkgs"), with the ISD row `A000000151000000`
  (#127) and the package AIDs recorded as the baseline signature:

  ```
  A0000000620202
  A0000000620204
  A0000001515350
  A00000016443446F634C697465   # …CDocLite preload
  ```

  (Registry seed — AIDs as recorded at baseline capture; re-derive any time
  via `gp -l` `PKG:` lines. A card showing a different registry is in a
  leaked test state → run the idempotent cleanup, never "fix" it by hand.)
- **INV-C2 — never speculative-auth.** SC-class commands (INITIALIZE
  UPDATE `80 50`, EXTERNAL AUTHENTICATE `84 82`, SC-gated GET DATA) are
  budget-bearing and NEVER looped; GP auth attempts are budgeted,
  ledgered, and one-per-new-keyset-with-provenance (AGENTS.md safe/unsafe
  table; #127). Tooling encodes this: the field recorder and the gempc-t1
  harness enforce the SC-class APDU guard at both the CLI parse and the
  library send boundary (PR #355).
- **INV-C3 — keys only to ledgered CPLCs.** The fixed org keyset goes only
  to cards whose CPLC matches the `J3R180_CPLC_ALLOWLIST` ledger
  (comma-separated hex prefixes of the CPLC value unwrapped from the `9F7F`
  TLV, read via plain `80CA9F7F`, no auth). Mismatch or unreadable CPLC
  aborts before any key-bearing exchange; `--allow-unverified-cplc` is the
  explicit operator escape hatch; an unset ledger degrades to the weaker
  ATR-family gate with a warning (branch `fix/codex-333-followups`, PR
  #341 — Codex P1 on #333). This registry (§1.2) is the intended value of
  that env var.
- **INV-C4 — locked is forever.** `50947320`'s ISD lockout has no standard
  recovery (#144); treat as terminal. The row exists so no future session
  spends budget rediscovering it.

## 2. Test manifests — one claim, one reproducible contract

A **claim** is any bench-result assertion this repo makes: a PR
description line, a CHANGELOG "hardware-verified", an AGENTS.md bench row,
an issue-closing statement. A **manifest** is the machine-checkable
contract for one claim. The manifest does not replace the runbook — it
*binds* a claim to the runbook's phase lines, the recorder's JSONL events,
and the firmware's escape counters.

### 2.1 Format

```yaml
id: HIL-M-0007                      # HIL-M-#### , assigned in filing order
claim: >                            # one sentence, falsifiable
  <what is asserted about hardware behavior>
claim_source: <PR #, issue #, CHANGELOG section, AGENTS.md row>
status: proven | gated | draft      # gated = proven scope + open gates listed

firmware:
  rev: <git sha | FWID string>      # what BOOTED — proven, never the flash log
  board_profile: <build.sh board / cargo feature set>

bench:
  host: ai-legion (192.168.13.208)
  labgrid_place: <place>            # tests/hardware/labgrid exporter config
  locks: [bench]                    # today: bench flock + pcscd flock
                                    # (tests/hardware/labgrid/bench.py,
                                    #  three-layer order);
                                    # field locks per PR #354 when it lands
reader:
  name: "<EXACT pcscd reader name>" # never substring, never index (runbook)
  config: </etc/reader.conf.d/...>  # how the reader reaches pcscd
card:
  registry_key: "<serial/batch>"    # §1.2 row
  cplc_prefix: "<hex>"              # INV-C3 gate value
  state_required: baseline          # INV-C1 pre-state

commands:
  - <exact command lines, in order>

expected_evidence:                  # ALL must be present for PASS
  runbook_phases:                   # exact phase() lines from the tool
    - "cleanup pre-existing OpenPGP artifacts"
    - "baseline registry captured (4 pkgs)"
    - ...
  recorder:                         # field_recorder.py JSONL (PR #355)
    - "recorder_meta … recorder_stop frame present"
    - "card_atr event with J3R180 family prefix"
    - "apdu_exchange events: request/response/SW + ms RTT recorded"
  escapes:                          # AGENTS.md bench table, m5stick row
    - "0xD0 counters sane (apdu_tx > 0, no reinit/error blowup)"
  exit_code: 0
  session_ledger: results/history.jsonl   # bench.py appends per session

gated:                              # open exceptions — amber, never silently red
  - "<issue #> — <phase> expected-skip with reason>"

sources:                            # every in-repo anchor this claim rests on
  - "<path (section) | PR # | issue #>"
```

### 2.2 Evidence binding and verdicts

- **Phase lines** are the lifecycle tool's own `phase("…")` strings —
  `cleanup pre-existing OpenPGP artifacts`, `baseline registry captured`,
  `install OpenPGP applet`, `OpenPGP SELECT via pcscd`,
  `gpg SCD SERIALNO (pinned reader)`, `uninstall OpenPGP artifacts`,
  `baseline registry restored` — and exit 0 means every contract phase
  green (`j3r180_gpg_lifecycle.py`; runbook §lifecycle-tool).
- **JSONL events** are the field recorder's append-only, one-event-per-line
  records (`ts`/`kind`/`reader`/`data`; kinds `reader_list`, `card_atr`,
  `apdu_exchange`, `poll_presence`; framed by `recorder_meta` /
  `recorder_stop` with per-kind counts; `--max-events` tcpdump-`-c`
  semantics — PR #355, `tests/hardware/nfc/field_recorder.py`). The
  recorder defaults to the ACR1252 **oracle** reader; `--reader` overrides
  onto a DUT reader. Killed runs keep their evidence (per-line flush).
- **Escape counters** come over the CCID wire itself (0xD0 diagnostics
  28-byte counters; 0xD2 turnaround decomposition via
  `tests/hardware/nfc/turnaround_probe.py`; 0xD3 poll cadence) — AGENTS.md
  m5stick bench row.
- **Boot proof**: FWID marker or Escape 0x02 identity string, never the
  flash log (AGENTS.md "Verify what booted").
- Verdicts: **PASS** (all expected evidence present) · **GATED** (proven
  phases green, open gates explicitly listed with issue refs) ·
  **SKIP-amber** (physical absence/place busy — SKIP ≠ FAIL, the #183
  nightly doctrine) · **FAIL** (code-caused).

### 2.3 Where manifests live

Inline in this doc until the format is exercised (this PR ships exactly
one worked example, §4). On adoption: `docs/hil/manifests/HIL-M-####.yaml`
plus a collection-time check in the `python-collect` gate class (stdlib
YAML parse + schema assert). Not built here — draft status is honest about
it.

## 3. Human-ops queue — the operations only a human can do

The agent can flash, probe, enumerate, and record. It **cannot** move a
card between coils, replug a cable, or look at hardware. These operations
recur (real precedents: the runbook's "pending physical card move" F469
row; AGENTS.md #88 "awaiting one hand-verified card removal"; the F746
"bring-up needs the physical USB plug" note; the 2026-10-17 nightly that
failed because the ACR1252 was physically off the bench and a human
replugged it by morning; the ESP32 erase-stranding class whose only
recovery is "physical replug + reflash").

### 3.1 The operation classes

| Op | What | Typical trigger | Post-op proof (machine-verifiable) |
|---|---|---|---|
| **HOP-1 card move** | Move a named card (by CPLC) onto/off a named coil or contact slot | manifest blocked on card placement (e.g. keyed card needed on the m5stick coil) | CPLC `80CA9F7F` read through the target reader matches the registry key; ATR family check; Escape 0xD0 presence bit settles |
| **HOP-2 cable replug** | USB/serial replug of a named device | FTDI/USB latch after the software ladder is exhausted (AGENTS.md wedge tables); board lent out and returned | pcscd/labgrid enumeration shows the device; by-id port resolves; FWID/identity probe answers |
| **HOP-3 physical inspection** | Human eyes on hardware the instruments cannot see (LCD render, coil coupling/position, card orientation, damage) | e.g. #193 "owner-eyes LCD check" for the display bring-up | operator ACK text (§3.2) + whatever probe exists (e.g. NT35510 read probe outcome); inspection-only ops may have no machine proof — the ACK *is* the evidence, so it must be explicit |
| **HOP-4 bench placement** | Device physically present/absent for the nightly (ACR1252 class) | nightly SKIP-amber on absence | exporter `avail: True`; next nightly green |
| **HOP-5 power intervention** | Power-cycle / re-seat after wedges the ladders cannot clear | erase-stranding, USB peripheral corruption | boot proof (FWID banner / identity escape) + first scripted session green |

### 3.2 Request / ack / post-verify protocol

All through the **kanban card** that owns the lane (the `t_…` id the PR
body references — same convention as PR #355's "Kanban **t_b8b5e0a4**"):

1. **Agent REQUEST** — a kanban comment, structured so a human can act in
   seconds without reading repo history:

   ```
   HOP-1 REQUEST (blocks HIL-M-0001)
   op: move card 50972320/6782 (CPLC prefix 4790d3214700000000003129)
       from: <current seat>  to: m5stick coil
   why: <one line — the manifest/claim that is blocked>
   safety: no GP auth will run before the CPLC gate re-verifies the card
   locks held: bench flock (release on your ack or on timeout)
   post-state I will verify: CPLC via m5stick reader == registry key,
   Escape 0xD0 presence settles within 30 s
   photo: optional (the machine check above is the required evidence)
   ```

2. **Human ACK** — a reply comment: **done** / **not done + reason**,
   timestamp, one line on anything anomalous seen by hand, photo optional.
   The photo is optional *because* the machine-verifiable post-state is
   mandatory — a photo can never substitute for the probe, and a probe
   can't see a cracked coil.

3. **Agent POST-VERIFY** — the agent re-reads hardware state by tool (the
   `post-state` line of the request), appends the session to
   `results/history.jsonl`, and records on the manifest: the ack's kanban
   URL + the post-verify probe output. Until POST-VERIFY passes, the op is
   *requested*, not *done* — "the human said so" is not evidence (same
   standard as "the flash log said so").

This mirrors the #321 recipe's discipline at the physical layer: the
pilot's step 1 already treated bench possession as a coordination fact to
state up front ("Bench is currently held… coordinate via
`/tmp/amperstrand-bench.lock` + lab tenant tasks first").

## 4. Worked example — HIL-M-0001 (m5stick J3R180 lifecycle)

The proven claim, from the runbook §Results (2026-10-20, firmware
`e9176577` == main `6382509c` content): the **m5stick serial reader
carries the full J3R180 GP card-management machinery** — SCP03 auth,
install 35 s, SELECT, uninstall, baseline restore — while applet *data*
operations are #330-gated (a device-side exchange bug, card variance
excluded by the #327 swap experiment).

```yaml
id: HIL-M-0001
claim: >
  The m5stick reader (our ESP32 firmware impersonating the GemPC Twin over
  libccidtwin) carries the complete J3R180 GP card-management lifecycle —
  ISD SCP03 auth with the org keyset, OpenPGP applet install (~35 s),
  pcscd SELECT, uninstall, and baseline restore.
claim_source: tests/hardware/nfc/J3R180-RUNBOOK.md §Results (m5stick row) + §Works
status: gated   # card-management machinery proven; applet data ops blocked by #330

firmware:
  rev: e9176577 (== main 6382509c content, per runbook §Results header)
  board_profile: m5stick (ESP32 + MFRC522, GemPC-Twin serial CCID)

bench:
  host: ai-legion (192.168.13.208)
  labgrid_place: m5stick            # serial DUT place (tests/hardware/labgrid)
  locks: [bench]                    # bench flock + pcscd flock (bench.py order)

reader:
  name: "M5stick GemPC (MFRC522) 00 00"
  config: /etc/reader.conf.d/m5stick-gempc    # libccidtwin.so, by-id port, :GemPCTwin

card:
  registry_key: "50972320/6782"
  cplc_prefix: "4790d3214700000000003129"     # INV-C3 gate (PR #341 ledger)
  state_required: baseline                     # INV-C1

commands:
  # preflight includes the CPLC ledger gate once PR #341 lands; today:
  # ATR-family gate + org-keyset single auth (budgeted, ledgered)
  - python3 tests/hardware/nfc/j3r180_gpg_lifecycle.py \
        --reader "M5stick GemPC (MFRC522) 00 00"
  - python3 tests/hardware/nfc/field_recorder.py \
        --reader "M5stick GemPC (MFRC522) 00 00" \
        --apdu 00A4040008A000000151000000   # SELECT ISD — pure read (#127 pattern)

expected_evidence:
  runbook_phases:                    # tool phase() lines, all PASS
    - "cleanup pre-existing OpenPGP artifacts"
    - "baseline registry captured"           # 4 pkgs (INV-C1 signature)
    - "install OpenPGP applet"               # ~35 s per runbook
    - "OpenPGP SELECT via pcscd"             # SW 9000
    - "uninstall OpenPGP artifacts"
    - "baseline registry restored"           # == captured registry
  recorder:
    - "recorder_meta / recorder_stop frame pair with consistent counts"
    - "card_atr: J3R180 family prefix (m5stick-negotiated 3B 85 81 31 FE 45 …)"
    - "apdu_exchange: SELECT SW=9000 with ms RTT recorded"
  escapes:
    - "Escape 0x02 identity = 'GemPC Twin ESP32 1.0' (boot proof)"
    - "Escape 0xD0 counters sane post-run (apdu_tx > 0, reinit stable)"
  exit_code: 0
  session_ledger: results/history.jsonl

gated:
  - "#330 — applet-function findings phase (GET DATA / PW1) expected to
     answer 6A88/6E00 through the m5stick path; the SAME card is fully
     functional through the nucula path (#327 revision). Amber, tracked."

sources:
  - tests/hardware/nfc/J3R180-RUNBOOK.md (§Results 2026-10-20, §Works, §reader matrix)
  - tests/hardware/nfc/j3r180_gpg_lifecycle.py (phase lines, baseline_pkgs)
  - PR #321 (protocol pilot — recipe shape)
  - PR #355 (field_recorder.py JSONL — in flight)
  - issue #327 (card-variance revision; swap experiment)
  - issue #127 (card ledger; key-budget history)
  - AGENTS.md (card-handling doctrine; m5stick bench row; lock rules)
```

Scope honesty: the ~35 s install figure and the phase set are the
runbook's 2026-10-20 evidence, on firmware `e9176577`; a *re-run* on a
newer rev is a new evidence bundle for the same manifest, not an edit of
the claim.

## 5. Adoption path (what must land before this stops being draft)

1. PR #341 (`fix/codex-333-followups`) — the `J3R180_CPLC_ALLOWLIST`
   ledger gate; this registry's rows become its value.
2. PR #355 — the ACR1252 field recorder; manifests' `recorder:` evidence
   keys off its JSONL.
3. PR #354 — field locks + `co-location.json`; the registry's volatile
   "seat" column becomes a checked invariant, and HOP-1 post-verify gains
   the drift check.
4. First adoption cycle: file manifests for the live claims (the runbook
   §Results rows), run them, and only then add the collection check (§2.3)
   — the protocol earns CI the same way #321 earned its merge.

## 6. Source index

| Anchor | What it grounds |
|---|---|
| `tests/hardware/nfc/J3R180-RUNBOOK.md` | lifecycle phases, reader matrix, keys, results incl. "4 pkgs" baseline, findings, "pending physical card move" |
| `tests/hardware/nfc/j3r180_gpg_lifecycle.py` | exact `phase()` lines, `baseline_pkgs()`, cleanup-before-baseline idempotence |
| `tests/soak/soak_02_globalplatform.py` | org dev keyset history (single source of truth chain) |
| PR #321 | the protocol pilot: merge-gated numbered hardware-verification recipe |
| Issue #127 | card inventory (serial/batch/pre-pers), retry budgets, read-only designation |
| Issue #155 | second-card CPLC capture format; not-in-ledger decision |
| Issue #327 | card-variance finding, ICFabricator 4790 cross-check, full-CPLC comparison queued |
| Issue #144 / AGENTS.md Card-Handling Doctrine | ISD-lock permanence; SC-class safe/unsafe table |
| PR #341 (branch `fix/codex-333-followups`) | `J3R180_CPLC_ALLOWLIST` gate semantics |
| PR #355 (`tools/acr-field-recorder`) | field recorder JSONL event schema |
| PR #354 (`docs/NFC-FIELD-LOCKS-DESIGN.md`) | field locks, `co-location.json` co-location invariant |
| `tests/hardware/labgrid/bench.py` + `tests/hardware/labgrid/README.md` | three-layer lock order, `results/history.jsonl` session ledger |
| `tools/hil/README.md` §9 | nightly bench runner; SKIP ≠ FAIL (#183), human-replug precedent |
| AGENTS.md (bench table, m5stick row) | Escape 0x02/0xD0/0xD2/0xD3 probes; F469/F746 physical-op precedents |
