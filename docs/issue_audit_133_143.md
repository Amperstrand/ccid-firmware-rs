# Issue Audit #133–#143 vs the tree (2026-10-12)

Provenance: issue-audit agent (Herdr-launched opencode session), read-only. Original at /tmp/opencode/issue-audit-133-143.md. Note: #137/#142-G03/T1-doc items were landed by sibling commits (e4c885e, 5823acc, dc835bb) shortly after this audit — the per-issue classifications below reflect main @ 46c4701.

# Issue Audit #133–#143 vs current tree (main @ 46c4701)

**Date:** 2026-10-12 · **Repo:** Amperstrand/ccid-firmware-rs, branch `main` (== origin/main, @ `46c4701` "fix(stm32): T=1 relay honest-error contract")
**Method:** `gh issue view` per issue + tree/git verification of every checklist item (issue-truth doctrine: issue bodies are leads, the tree is evidence).
**Note:** `gh issue view --comments` currently fails with a GitHub GraphQL error (Projects-classic deprecation on `projectCards`) — comment-level cross-refs could not be re-read; commit-message cross-refs were used instead.

---

## Cross-cutting findings (read first)

1. **`reference/CCID` is a FULL git submodule, not a trimmed copy.** `.gitmodules` pins `salsa.debian.org/rousseau/CCID.git` @ `1a0f75d` (1.7.1-8). `ifdhandler.c`, `parse.c`, `defs.h`, `debug.*`, `misc.h`, `simclist.*`, `create_Info_plist.pl`, `Info.plist.src`, `openct/` — everything #136 lists as "missing" — **is present in the checkout and has been since the submodule landed in `b79d09a` (2026-03-15)**. #136's premise ("trimmed vendored copy, missing files") is factually wrong for this tree; its vendoring task is already satisfied by the submodule (provenance = the gitlink pin).
2. **Branch divergence relevant to #133:** `sisyphus/idf-555-unify` (unmerged, `git log --all --not main`) carries `29dab9c` (log_shim retired, #131; **adds `docs/LOGGING_DOCTRINE.md`**) and `6c95e2f` (defmt-is-cheap-not-free doctrine). The `test_wire_silence_after_banner` test #133 cites as the existing lock lives **only on that branch** (`tests/hardware/nucula/test_nucula.py:208` there; absent on main). #133's premise is branch-state, not main-state.
3. **Two docs went stale the moment `46c4701` landed** (2026-10-12 09:33+0200, ~3 min after the issues were filed 07:30Z):
   - `docs/T1_BEST_PRACTICES.md` line 239 still says waiting times are "**Not enforced**: fixed 500 ms first-byte / 50 ms inter-byte" — the first-byte half is now false (`smartcard.rs:757-764` derives from BWT). G-07 needs a resolution update (#137 item).
   - The same doc's tables still mark G-01 (R-block PCB bits) ❌ — but `7f136a0` (2026-10-10, *before* the issues were filed) fixed `parse_pcb`/`i_pcb`/`r_pcb` to Table 45 layout, bench-verified (commit message: byte-exact 0x10/0x40 answers, 10/10 pcscd APDUs SW=9000).
4. **46c4701 scope (verified in tree):** `t1_bwt_ms` + tests in `smartcard_common.rs:50-92`; BWT-derived first-byte wait `smartcard.rs:757-764` (`clamp(500, 5_000)`); honest errors (`Err` on first-byte timeout `:692`, truncation → `ProtocolError` `:706-712`, ORE fails exchange `:772-778`); `prepare_rx` drain `:663`; root-cause doc `docs/t1_relay_132_rootcause.md`; HIL touched (`test_ccid_hil.py`, `test_serial_battery_hil.py`).
5. **CCID-layer error mapping:** driver `Err` arms map to FAILED + `CCID_ERR_ICC_MUTE` (`crates/ccid-core/src/handler.rs:471,520,561,569,659`, test `:2147`). There is **no parity→`0xFD` mapping anywhere** (grep: no 0xFD/PARITY in ccid-core or firmware) — and PE/FE/NE-corrupted bytes are still **accepted as data** with only a defmt warn (`smartcard.rs:780-785`).

---

## Per-issue audit

### #133 — Observability unification (escape registry, trace ring, defmt policy, silence tests)
**Classification: OPEN** (on main; PARTIAL if the unmerged branch counts).

Evidence per checklist item (main @ 46c4701):
- Escape registry module: **absent** — no `0xD4`/`TraceRing`/trace-ring code anywhere (grep over `crates/`+`firmware/`).
- Versioned 0xD0 envelope: **absent** — no version byte in `crates/ccid-core/src/diagnostics.rs` (grep "version": 0 hits).
- Trace ring + Escape 0xD4: **absent**.
- STM32 defmt audit: **not done** — `smartcard_bitbang.rs` carries 16 `defmt::` records, placement never audited; `receive_byte_timeout` (`smartcard.rs:766-798`) defmt-warns from the hot RX loop (event-driven, but unaudited policy-wise).
- m5stick wire-silence HIL: **absent on main** — `test_wire_silence_after_banner` exists only on `sisyphus/idf-555-unify` (nucula, not m5stick).
- Broker/battery envelope consumption: absent.
- Doctrine cross-refs: `docs/LOGGING_DOCTRINE.md` **not on main** (added by `29dab9c` on the branch).

**Dependencies:** follow-up to #130/#131; orthogonal to the #134 umbrella. The #131 half (log ceiling) is done *on the branch* — merging/settling `sisyphus/idf-555-unify` is a precondition for starting #133 cleanly (otherwise main and the doctrine diverge further).
**Effort/impact:** **M** — registry is a pure host-testable refactor (S); envelope versioning needs host-tool coordination (S-M); trace ring + 0xD4 + decoder is new capability (M) + bench validation. Bench: yes for the HIL silence test and trace-ring validation, no for registry/envelope.
**Prerequisite for (a)/(b):** neither directly — but it is the debugging lever for *both* (a "what sequence led here" recorder is exactly what the #101/#132 class needed).

### #134 — Reference-first conformance audit program (UMBRELLA)
**Classification: PARTIAL.**

Done in tree:
- Spec-quote CI **exists** as a dedicated workflow `.github/workflows/spec-quote-drift.yml` (push/PR/weekly; strict coverage gate for CCID_SPEC/CCID_SERIAL/GEMPC; PCSC/LIBCCID/ISO7816 drift-checked but not yet strict — header comment documents exactly this). The issue's "CI step" item is effectively addressed for 3 of 6 sources.
- `[sources.LIBCCID]` (commands.c), `[sources.PCSC]` (MacOSX/reader.h), `[sources.ISO7816]` (osmo iso7816_3.c) are wired in `specquotes.toml` (2026-10-11 audit wave).

Open: vendoring-policy doc (the 3-class policy is in the issue, not in `docs/`), `reference/README.md` provenance index (absent), orphan-marker sweep, lane cross-link discipline.
**Dependencies:** umbrella of #135–#143 lanes (its own body lists them). #133 is *not* a lane.
**Effort/impact:** residual **S** (docs + index + sweep); no bench. Umbrella tracking overhead is real but it encodes the doctrine that already paid off (#132 root cause was found by exactly this method).
**Prerequisite for (a)/(b):** process-level for both; not a hard technical blocker.

### #135 — Vendor ST reference + fix F4 smartcard framing (STOP, GT, PE/ORE contract)
**Classification: OPEN** (one sub-slice of one item landed via 46c4701).

- `reference/ST/` does **not exist** — vendoring not started.
- The STOP-bit violation is **still live**: `smartcard.rs:419-431` switches T=1 to STOP=1 ("CR2 bits 13:12: 00 = 1 stop bit") with GT=1, vs ST HAL `IS_SMARTCARD_STOPBIT` (0.5/1.5 only). Register-value unit-test lock: absent.
- RX error contract: **ORE slice done** (fails exchange, `smartcard.rs:772-778`), but PE/FE/NE bytes are still returned as data (`:780-785`), no Diagnostics counting of PE/ORE, no bError `0xFD`.
- NACKEN=1 evaluation: not started.

**Dependencies:** lane of #134; consumes #140's EMV proxy for the guard-time/NACKEN citation; the 0xFD host contract half lives in #136.
**Effort/impact:** **M** — vendoring+specquotes (S), STOP/GT register fix + lock test (S-M), PE/ORE diagnostics + 0xFD (M, touches firmware + ccid-core), bench confirm with RTT watch + full HIL (bench **required**).
**Prerequisite for:** (a) **yes, soft-but-real** — this is the electrical-fidelity debt on the exact wire the Teapot CAP secure channel rides (content-dependent corruption class from the #132 dig; the 0/400 fidelity hammer says it is not currently firing, but it's unfixed nonconformance on the (a) path). (b) no (different silicon/frontend).

### #136 — libccid completion + reference-correct CCID error mapping
**Classification: PARTIAL, with one STALE premise item.**

- ~~Vendor ifdhandler.c/parse.c~~: **already satisfied** — full submodule @ `1a0f75d` (see cross-cutting finding #1). What's genuinely missing from this item is only the `reference/README.md` provenance index (tracked under #134 anyway).
- Specquotes wiring: `[sources.LIBCCID]` points at `commands.c` only; no `ifdhandler.c` quotes, no `[sources.OPENCT]` (proto-t1.c is tracked at `reference/CCID/src/openct/` but unwired).
- Firmware error mapping: **half landed by 46c4701** — never-`Ok(short)` contract done (`smartcard.rs:686-712`), driver `Err` → FAILED+`ICC_MUTE` (`handler.rs:520/569/659`, test `:2147`). Parity→`0xFD`: **absent** (and blocked on #135's PE detection). Fuzz-suite arms for the new error shapes: partial (46c4701 included host tests; 0xFD arm impossible until it exists).
- H4 study (SetDataRate/PPS-via-XfrBlock conditions for our dwFeatures): **not done** — doc-only work that settles a #132 hypothesis without bench time.

**Dependencies:** lane of #134; pairs with #135 (error contract) and #138/#143 (parse.c usage).
**Effort/impact:** residual **M**; bench not strictly required (fuzz + HIL re-run only).
**Prerequisite for:** (a) soft — the error shapes pcscd sees during GP retries; (b) soft — same class on the serial path.

### #137 — T=1/T=0 timing contract (kill fixed 500/50 ms) (#132 H1)
**Classification: PARTIAL — the core fix landed.**

Done (46c4701, verified): `bwt_ms` host-buildable helper + unit tests incl. BWI=4→1362 ms and defensive caps (`smartcard_common.rs:50-92`); first-byte timeout derived from negotiated BWT at current (PPS) baud, floored 500 ms / capped 5 s (`smartcard.rs:757-764`); `prepare_rx` drain; bench fidelity hammer 0 anomalies/400 (vs 54/446 pre-fix); HIL suite green (14+1).

Open:
- Inter-byte from CWT: **still flat 50 ms** (`smartcard.rs:683,699`).
- Invariant test "reader timeout ≥ advertised BWT for every GetParameters echo": **absent** (only formula/monotonic tests exist; note the `clamp(500,5_000)` cap technically breaks the invariant for advertised BWT > 5 s, i.e. BWI ≥ 10 — worth encoding as a test to force the cap decision).
- T=0 WWT derivation/documentation: absent (flat procedure timeout remains).
- G-07 doc update: **absent and now actively stale** (cross-cutting finding #3).
- Stage-1/stage-3 #132 evidence: bench-gated on the card swap.

**Dependencies:** lane of #134; consumes #140's EMV tables; feeds #132's closure evidence.
**Effort/impact:** residual **S-M** (CWT helper + invariant test + T=0 doc + G-07 rewrite are all cheap host/doc work; stage evidence is bench-gated).
**Prerequisite for:** (a) **largely satisfied already** — the BWT race was the (a)-path killer and it's fixed; residual CWT risk is low (50 ms flat is generous vs typical CWT). (b) no.

### #138 — ST USB CCID middleware vendoring + descriptor/wire conformance audit
**Classification: OPEN.** No `reference/ST/`, no parse.c-derived check anywhere in CI, no descriptor audit table in `docs/`. Nothing started.
**Dependencies:** lane of #134; consumes #136's parse.c presence (already satisfied); #143 implements its parse-in-CI item.
**Effort/impact:** **M-L** — vendoring (S), building/porting parse.c checks as a host test (M — C with deps; a Rust port of the checks is the realistic route), field-by-field audit vs ST template + readers/*.txt (M), ZLP/endpoint study vs `ccid_usb.c` (M). Bench: yes for any descriptor value change (pcscd + battery), no for the audit itself.
**Prerequisite for:** neither (a) nor (b) directly — hardening/conformance, not a functional blocker.

### #139 — GlobalPlatform conformance lane + #132 decisive experiment
**Classification: OPEN** (experiment *staged*, not run — bench-gated).

- `docs/gp/` absent — layout doc not started.
- The #132 decisive experiment (stages 1–3 per root-cause report §3) is staged in `docs/t1_relay_132_rootcause.md` but awaits the P71 card swap into the STM32 slot (AGENTS bench state).
- Capture runbook (`tests/hardware/captures/`) absent — overlaps #143.
- In-repo assets that de-risk it: AUTH triple + retry budgets (issue #127 inventory), gp.jar + CAPs cached at /tmp/opencode/gp/.

**Dependencies:** lane of #134; the experiment depends on bench card placement (external, not another issue); runbook tooling overlaps #143.
**Effort/impact:** **M** — gp.java/spec study (M, desk), experiment (S once the card is in place, but consumes retry budget — no improvised attempts), runbook (S).
**Prerequisite for:** **(a) YES — this is the lane**: the Teapot CAP lifecycle through the Cherry/STM32 reader is exactly the #132-proof + GP-understanding path. (b) no (m5stick CAP path is esp32-side).

### #140 — Spec-source policy (free proxies for paywalled ISO)
**Classification: OPEN.** `docs/quote-sources/` does not exist; no EMV Book 1 extraction; no proxy-ladder README; no unverified-numeric-claims audit pass.
**Dependencies:** lane of #134 and a **supplier** for #135 (guard-time/NACKEN citations), #137 (CWT/BWT/BGT tables), #139 (GP spec quotes).
**Effort/impact:** **S-M**, zero bench. High leverage per unit effort: it is the anti-"founding incident" lane (stops agents guessing spec bytes ever again).
**Prerequisite for:** neither directly; enables #135/#137/#139 which are (a)-relevant.

### #141 — PC/SC host-contract audit vs pcsc-lite
**Classification: OPEN.** No pcsc-lite subset vendored (`reference/` = CCID + osmo only; the tracked PCSC source is the CCID submodule's `MacOSX/reader.h`); no attribute/feature audit table; no pcsc_scan snapshots in `tests/hardware/`.
**Dependencies:** lane of #134; adjacent to #136 (both host-contract).
**Effort/impact:** **M**; bench: cheap snapshots only.
**Prerequisite for:** neither. Hygiene + future-proofing (PIN structures already partially quote-locked via [sources.PCSC]).

### #142 — esp32 NFC lane: quote-lock NCI/14443, close G-01/G-03/G-04
**Classification: PARTIAL — G-01 landed before the issue was filed.**

- **G-01 (R-PCB bits): FIXED** by `7f136a0` (2026-10-10): `parse_pcb` R nr now `(pcb>>4)&1`, `r_pcb` now `0x80 | (nr<<4) | err` (`t1.rs:71-75,109-111`), bench-verified byte-exact (0x90-class answers). Residual nit: the round-trip tests assert against `r_pcb(...)` itself (self-consistent, `t1.rs:414,436,452,504`) — the byte-literal lock (`assert r_pcb(1,0)==0x90`) the doc recommends is still worth one small test. The doc's own G-01/❌ tables are now stale (finding #3).
- **G-03 (254-byte truncation): STILL LIVE** — `ccid_handler.rs:266-268`: `let mut apdu_buf = [0u8; 254]; let an = card_apdu.len().min(apdu_buf.len());` silently clamps. GP STORE DATA > 254 B (CAP install!) is still corrupted with a success report.
- **G-04 (WTX detached from BWT): STILL LIVE** — `t1.rs:316-321`: `if took_ms > 150 { … S(WTX request 2) }` (fixed m=2, magic 150 ms); SetParameters T=1 still answered with fixed `DEFAULT_T1_PARAMS` (accept-and-forget).
- NCI/14443 quote completion + citation-map extension: not started.

**Dependencies:** lane of #134; G-04's fix consumes #140's timing tables.
**Effort/impact:** **M** — G-03 fix + buffer test (S, host-testable, immediate), G-04 (M: parse SetParameters T=1 + BWT-derived m, bench), quote-locking grind (M, no bench).
**Prerequisite for:** **(b) YES — direct**: G-03 corrupts exactly the CAP-install STORE DATA traffic; G-04 aborts sessions during on-card crypto; (G-05 — synthetic ATR lacking TA3/TB3, forcing host ifsc=32 and 8× chain round-trips — is in the doc but *not* in #142's close-list; treat it as the hidden third prerequisite and fold it into this lane). (a) no.

### #143 — Conformance harness (parse fixture in CI, capture corpus, byte-diff)
**Classification: OPEN.** No `tests/hardware/captures/`, no capture runner in `host-tools/`, no byte-diff utility, no contact-reader differential (Cherry vs CardMan APDU agreement — `perf_ab.py` compares latency only; the nfc differential is contactless-only). parse.c not exercised anywhere.
**Dependencies:** lane of #134; builds on #136 (parse.c present), implements #138's CI item, generalizes #139 stage-2; "extends #92/#87" per its own body.
**Effort/impact:** **M-L**; bench required for the capture corpus and contact differential.
**Prerequisite for:** (b) soft-but-valuable (tagged captures + byte-diff is the debugging instrument the m5stick CAP stall needed); (a) soft (the same instrument for the decisive experiment).

---

## Dependency & ordering map

```
#134 (umbrella: policy/CI/index)
 ├── #140 (spec proxies) ── feeds ──> #135 (EMV guard-time), #137 (WT tables), #139 (GP spec)
 ├── #136 (libccid quotes + error map) <─ pairs ─> #135 (PE→0xFD contract)
 │        └── already-satisfied vendoring (submodule); feeds #138/#143 (parse.c)
 ├── #137 (timing; core fix LANDED 46c4701, residuals cheap)
 ├── #138 (ST USB middleware + descriptors)  <── #143 implements its parse-in-CI item
 ├── #139 (GP lane + #132 decisive experiment)  <── bench card swap (external)
 ├── #141 (pcsc-lite host contract)
 ├── #142 (esp32 NFC; G-01 done pre-filing, G-03/G-04 open)  <── G-04 needs #140 tables
 └── #143 (harness/captures/byte-diff)  <── generalizes #139 stage-2, extends #92/#87

#133 (observability) — NOT a #134 lane; follow-up to #130/#131;
   blocked-ish on merging sisyphus/idf-555-unify (LOGGING_DOCTRINE, silence test, log_shim retirement)
```

## Prerequisite matrix

| Issue | (a) Teapot CAP via Cherry/STM32 | (b) m5stick/nucula CAP-install (#59/#101) |
|---|---|---|
| #133 | neither (debugging lever for both) | neither (lever) |
| #134 | process | process |
| #135 | **YES (soft-real)**: electrical fidelity on the secure-channel wire | no |
| #136 | soft (pcscd-visible error shapes) | soft |
| #137 | mostly satisfied (46c4701); residual low-risk | no |
| #138 | no | no |
| #139 | **YES — is the lane** | no |
| #140 | enabler (feeds #135/#137/#139) | enabler (feeds #142/G-04) |
| #141 | no | no |
| #142 | no | **YES — direct** (G-03 corrupts STORE DATA, G-04 kills WTX; fold in G-05) |
| #143 | soft (experiment instrument) | soft (debug instrument) |

External gates outside this range: (a) also needs the P71 card physically in the STM32 slot + #132 proof; (b) also needs the #59 MFRC522 per-block perf pass and the #101 serve-loop hardening.

## Recommended execution order

**Phase 0 — housekeeping / truth repair (no bench, ≤ half session):**
1. **Settle `sisyphus/idf-555-unify`** — push/merge per the worktree-farm rule; #133 cannot start on main while its doctrine, silence test, and log_shim retirement live on a side branch.
2. **Doc-truth repairs:** update G-07 + G-01 entries in `docs/T1_BEST_PRACTICES.md` (they now contradict the tree); add the byte-literal `r_pcb(1,0)==0x90` lock test while there (#142 residual nit, 15 min).
3. **#134 residuals:** `reference/README.md` provenance index + vendoring-policy doc + orphan-marker sweep (S; also closes #136's only legitimate vendoring gap).

**Phase 1 — quick host-side wins (no bench, 1–2 sessions):**
4. **#142 G-03**: size the relay buffer past 254 with an assert/error instead of silent `min()` + host test (S). *Direct CAP prerequisite; smallest impact/effort ratio in the whole batch.*
5. **#136 residuals:** wire `[sources.OPENCT]` + ifdhandler quotes; write the H4 desk study (settles a #132 hypothesis for free).
6. **#137 residuals:** CWT helper + inter-byte derivation + invariant test + T=0 margin doc (S-M).
7. **#140**: proxy-ladder README + EMV timing-table extraction (S-M) — do before #135/#142-G-04 citations are needed.

**Phase 2 — bench-gated (needs places/cards):**
8. **#139 / #132 decisive experiment + Teapot lifecycle run** through the Cherry reader — *the* (a) milestone; gate on the card swap; respect retry budgets. Highest impact item in the batch.
9. **#142 G-04 (+G-05 fold-in)** on the m5stick bench — the (b) protocol unlock alongside #59 perf.
10. **#135** STOP/GT/NACKEN + PE/ORE→0xFD with RTT watch, then full HIL.
11. **#143** capture corpus + byte-diff + contact differential (instrument for everything above; schedule once Phase-2 work starts producing captures).
12. **#133** registry → envelope → trace ring (host-side start anytime after Phase 0; bench for silence HIL + ring validation).
13. **#138 / #141** — background conformance grind, lowest urgency.

**Bench-gated markers:** #139 (card swap + retry budget), #142 G-04/G-05 (m5stick place), #135 (RTT watch + full HIL), #143 (captures + differential), #133 final validation (silence HIL). Everything in Phases 0–1 is desk work.
