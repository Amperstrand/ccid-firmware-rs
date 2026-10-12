# T=1 Relay Root-Cause Dig — #132 (2026-10-12)

Provenance: read-only investigation by the t1-dig agent (Herdr-launched, opencode).
Original at /tmp/opencode/t1-dig-report.md; findings implemented same day in
smartcard.rs (BWT-derived first-byte timeout, pre-read drain, honest-error
contract, ORE hygiene) — see CHANGELOG Unreleased and #132.

# #132 Root-Cause Dig — Card-Specific GP SCP02 Failure Through the Cherry (STM32F469) Reader

Read-only investigation, 2026-10-12. No files were modified. Branch `main`.

**Bench facts (given):** Card A (CPLC serial `50 94 73 20`) auths with the AUTH triple through our
Cherry reader 3/3; Card B (`50 94 27 20`, same ATR `3B D5 18 FF 81 91 FE 1F C3 80 73 C8 21 10 0A`,
same empty-FCI SELECT, same 28-byte INITIALIZE UPDATE shape) fails deterministically 2/2
(incl. a second keyset) through Cherry, succeeds through ACR1252. Differing bytes in the INIT
UPDATE response: KDD offset 6 (`0x73` vs `0x27`) and the random challenge/cryptogram fields.

---

## 0. Correction to the bug report's architecture premise (load-bearing)

**The F469 Cherry build is NOT "Short APDU exchange level with firmware-terminated T=1".**
It is a **TPDU-level transparent byte pipe**: the host's libccid runs the full T=1 engine
(openct `proto-t1.c`) and our firmware forwards whole blocks as XfrBlock payloads.

Evidence:

| Claim | File:line |
|---|---|
| All three profiles advertise `FEAT_LEVEL_TPDU` (dwFeatures bits 16..18 = 0x0001_0000) | `firmware/ccid-firmware/src/device_profile.rs:404-410, 471, 501, 541`; asserted in tests at `device_profile.rs:622-635` |
| `AUTO_IFSD` deliberately NOT set — libccid runs its own S(IFS) dance through the pipe | `device_profile.rs:610-611` area (test `test_cherry_st2100_features_no_auto_ifsd`, line 622) |
| XfrBlock → `driver.transmit_apdu` → **protocol==1 → `transmit_raw` (raw byte pipe)** | `firmware/ccid-firmware/src/smartcard.rs:619-620`, `smartcard.rs:631-722` |
| Response side: reads exactly one card block via its T=1 prologue LEN | `smartcard.rs:661-698`, helper `t1_block_total_len` at `smartcard_common.rs:34-44` |
| `t1_engine.rs` (a real T=1 initiator) has **zero callers** — dead code | grep: only `mod t1_engine;` (`main.rs:51`) and the `T1Transport` impls (`smartcard.rs:787`, `smartcard_bitbang.rs:718`) |
| The core handler relays the payload verbatim and copies the response verbatim | `crates/ccid-core/src/handler.rs:647-722` (`handle_xfr_block_contact`), `write_message` copy at `crates/ccid-core/src/lib.rs:97-125` |
| `docs/T1_BEST_PRACTICES.md` §5.3 documents this exact model ("model B: transparent TPDU pipe", commit `b1e685a`) | `docs/T1_BEST_PRACTICES.md:43, 223-241` |

**Consequence (the single most important analytical result of this dig):**

> ### The LRC wall
> In the pipe model, **libccid verifies the LRC of every block the card sends** and the **card
> verifies the LRC of every block we forward**. Therefore a *simple* corrupted / dropped /
> duplicated byte in either direction **cannot silently reach gp's crypto**: it produces an
> R(EDC) → retransmission → resync ladder → pcscd-visible comm error. A "clean" host-cryptogram
> mismatch requires corruption that is **LRC-invisible** (byte *permutation* — XOR is
> order-independent — or delivery of a *valid but wrong* block for the wrong exchange), or a
> **timing/state desync** that mis-associates a genuine card response with the wrong host
> exchange, or the observed failure is actually comm-class (and the bench wording paraphrased
> it). Every hypothesis below is scored against this wall.

---

## 1. Full receive-path map (card → host) with per-hop verdicts

```
Card I/O pin (PA2, open-drain, internal pull-up, High speed)      board_config.rs:41-45
  → USART2 smartcard mode RX (9-bit, even parity, SCEN=1, NACKEN=0 for T=1)
      init CR1=0x340C CR2=0x3900(1.5 stop) CR3=0x0020             smartcard.rs:127-136
      T=1 switch: STOP=1 bit, GTPR.GT=1                           smartcard.rs:412-424
      wire: card clock 4.2 MHz (presc=5 of 42 MHz PCLK1), Di=1 → 11290 baud, ETU≈88.6 µs
      (PPS speed-up disabled on main: PPS_APPLY_NEGOTIATED_BAUD=false)  smartcard.rs:37, 248-265
  → receive_byte_timeout(): poll SR; RXNE **or ORE** → read DR, return byte.
      PE/FE/NE/ORE only logged (warn), byte still accepted.        smartcard.rs:724-748
  → transmit_raw() T=1 branch: read 3-byte prologue (500 ms first byte, then 50 ms),
      expected = LEN+4, read to expected (50 ms inter-byte), Timeout ⇒ break/return-Ok(short)
                                                                   smartcard.rs:661-698
  → handler core: tx_buf[0..resp_len] → RDR_to_PC_DataBlock        crates/ccid-core/src/handler.rs:693-705
  → write_message verbatim copy into tx_buffer                     crates/ccid-core/src/lib.rs:97-125
  → USB: try_send chunks ≤64 B, ZLP rule, single-packet at these sizes
                                                                   firmware/ccid-firmware/src/ccid.rs:171-210
  → host libccid T=1 engine: parses block, **verifies LRC**, extracts R-APDU → gp
```

Buffer-size arithmetic for the failing exchange (rules out a whole suspect class):
INIT UPDATE response = 28 data + SW 2 = 30 INF → block = 3 + 30 + 1 = **34 bytes**;
IFSC=254 (TA3=0xFE parsed at `smartcard_common.rs:194-196`), our announced IFSD=254
(`do_ifs_negotiation_t1`, `smartcard_common.rs:290-320`), so **no chaining**; response
buffer is 271 B (`handler.rs:96`), CCID message 44 B = one 64 B USB packet. No boundary
is crossed anywhere in this exchange.

ATR parse of the shared ATR (both cards): TD1=0x81→TD2=0x91→TA3=0xFE (IFSC 254);
**no TB3, no TC3** → LRC EDC; our parser defaults bwi=4/cwi=13 (`smartcard_common.rs:164-172`)
which we echo in GetParameters (byte 3 = 0x4D) → the host engine runs a patient
BWT ≈ 1.38 s. All ATR-derived parameters are **identical** for cards A and B — the *only*
differences in this session are response **content** and the card's own **timing**.

---

## 2. Ranked hypotheses

### H1 (TOP) — 500 ms first-byte timeout + `Ok`-on-timeout contract: orphaned-block / stale-byte contamination chain (timing-dependent, card-specific, deterministic)

* **Mechanism.** `transmit_raw`'s T=1 branch gives the card **500 ms** to start its block and
  **50 ms** between bytes, and on timeout returns `Ok(total_len)` — an **empty or truncated
  DataBlock reported as success** (`smartcard.rs:662-677`: `Err(Timeout) => return Ok(total_len)`;
  `smartcard.rs:680-688`: `Err(Timeout) => break` — "truncated block"). If card B (older CPLC
  mask) answers INIT UPDATE / EXTERNAL AUTH in > 500 ms — legal per T=1 if within BWT, and our
  own GetParameters echo tells the host BWT≈1.38 s — then:
  1. We bail at 500 ms and hand libccid an empty/short "success".
  2. libccid's engine read fails → it retransmits (R-block / same I-block) via a new XfrBlock.
  3. Meanwhile the card's orphaned 34-byte block arrives **with nobody reading the UART**
     (the RX path is only read inside `transmit_raw`, never from the USB main loop,
     `main.rs:870-1114`): DR takes byte 1, every subsequent byte sets **ORE and is discarded**.
  4. `transmit_raw` has **no pre-TX drain** (`smartcard.rs:648-651` — contrast
     `T1Transport::prepare_rx` at `smartcard.rs:795-812`, which exists but is only used by the
     dead `t1_engine`): the stale byte (and the ORE flag) is still in DR when the *next*
     exchange begins, and `receive_byte_timeout` **accepts ORE reads as data**
     (`smartcard.rs:730-735`). The next response's prologue is shifted → LEN misread →
     garbage or stitched blocks.
  5. Depending on interleaving, libccid ends up pairing a **valid card block with the wrong
     host exchange** (a late response to attempt #1 delivered as the payload for the R-block/
     retry of attempt #2). SCP02 is self-consistent per response, but the host challenge
     differs per gp retry → the card's cryptogram is over H1 while gp checks against H2 →
     **clean mutual-auth failure with no byte corruption at all**. This is the one interleaving
     that crosses the LRC wall legally.
* **Why A passes, B fails:** the timeouts were introduced and validated **only against the
  fast ComSign eID** in perf commit `13b17c8` ("kill three fixed latencies", 2026-10-10 —
  message: benchmarked 300 iters on ComSign T=1). Card A (newer mask `…7320`) answers in
  tens of ms; card B (older mask `…2720`) can deterministically exceed 500 ms on the
  3DES/MAC path. No content dependence needed — per-card timing is enough, and it stays
  deterministic across attempts and keysets.
* **Signature it must leave (testable with zero auth attempts):** defmt
  `"Rx timeout 500ms SR=…"` (`smartcard.rs:739`) and/or `"USART err SR=… byte=…"` (ORE,
  `smartcard.rs:733`) in the RTT log during the failing gp run; R-block/resync activity in the
  libccid trace.

### H2 — Content-dependent physical-layer corruption accepted through the PE/ORE-tolerant reader (the 0x27-vs-0x73 class)

* `receive_byte_timeout` returns the byte even when SR shows PE/FE/NE/ORE (warn only,
  `smartcard.rs:730-735`). T=1 has **no character-level retry** (NACKEN=0 is correct for T=1,
  `smartcard.rs:412-424` — GT=1 makes NACK impossible by design), so a parity-corrupted byte
  *should* flow to the block layer — but the block layer that must catch it is the **host's**,
  and the corrupted byte then costs an R-block round-trip. Deterministic per-content bit
  corruption (e.g. open-drain rise-time marginal sampling on long LOW runs) is real:
  `0x27` on the wire (LSB first `11100100`, parity 0) contains a **3-bit LOW run**
  (data bits 4-5 + parity), while `0x73` (`11001110`, parity 1) maxes at 2 — a slow pull-up
  recovery deterministically distorts exactly the class of byte card B carries in its KDD.
  *But* any flipped data bit breaks the card's LRC → host R(EDC) → same corruption on every
  retransmission → resync ladder → **comm-class failure**, which contradicts the bench wording
  unless the bench paraphrased a comm failure as "cryptogram mismatch". Rank #2 because the
  physics fits the 0x27/0x73 observation beautifully, but the LRC wall pushes its visible
  failure mode into comm errors.
* **Zero-auth signature:** repeated `"USART err SR=0x…4 (PE)"` / `SR=0x…8 (ORE)` warns plus
  host-side R-blocks.

### H3 — ORE-shredded stream accepted as a prologue byte (drop mechanism, feeds H1)

`receive_byte_timeout` treats ORE exactly like RXNE (`smartcard.rs:730-735`): after an
overrun the DR holds the *oldest* unread byte while all newer bytes were discarded — reading
it as fresh data shifts every subsequent byte. This is the amplifier that turns H1's orphaned
block into a *prologue misparse* (LEN read from a data byte → wrong `expected` → stitched
truncation). Any prior exchange that left ≥1 unread byte (IFS-negotiation partial reads at
power_on — `do_ifs_negotiation_t1` returns Err after 3-4 bytes with no tail drain,
`smartcard_common.rs:303-319`; a previous CWT-broken block) seeds the same contamination.

### H4 — Mid-session `SetDataRateAndClockFrequency` rewrites BRR (baud desync → content-dependent sampling)

`handle_set_data_rate_and_clock` (`firmware/ccid-firmware/src/ccid_core.rs:322-382`) writes
BRR directly from the host-supplied rate, unconditionally, any time the host asks. The Cherry
descriptor advertises dwDataRate=10753 while the actual ATR-default wire is 11290 baud
(4.2 MHz/372): if any host tooling sends that request mid-session, the reader steps **4.7%**
away from the card — inside nominal UART tolerance but at the edge where *specific bit
patterns* mis-sample, deterministically. Cherry's dwFeatures (0x000100BA: auto-param,
auto-voltage, auto-clock, auto-baud, auto-PPS) means stock libccid should never send it —
**check the capture for message type 0x73 before spending effort here** (bench scripts are a
plausible sender). `smartcard.rs:189-193` only guards this when `pps_speedup_active`, which is
always false on main.

### H5 — T=1 character framing config (STOP=1 + GT=1) marginally out of RM0090's smartcard expectations

`smartcard.rs:414-424` sets 1 stop bit + guard time 1 for T=1. RM0090's smartcard mode
specifies 0.5/1.5 stop-bit support; the config demonstrably works (ComSign eID, card A,
labgrid 7/7) but shaves the character timing to its margin, which is exactly the regime where
per-pattern sampling (H2) starts to matter. Not a standalone cause; a contributing margin.

### H6 — LRC-invisible permutation / stale-buffer delivery inside our firmware (residual)

Searched exhaustively: RX is strictly sequential (`response[total_len++]`), the core copies
verbatim (`handler.rs:693-705`, `lib.rs:97-125`), USB chunks in order (`ccid.rs:171-210`);
`rx_buffer` stitching is impossible in practice because libccid is strict
request/response (`ccid.rs:233-262` reads one OUT packet per poll; `feed`/`message_ready`
`ccid_core.rs:147-166`). **No permutation site exists in our code.** This hypothesis survives
only if the decisive capture (§3) shows a byte diff in a *host-accepted, LRC-valid* block with
*no* R-blocks — in that case escalate to Escape 0xD1 coredumps + on-device response logging
and look at the vendored `synopsys-usb-otg` patch. Lowest prior; kept because it is the only
class that crosses the LRC wall without H1's mis-association.

### Explicitly ruled out (with reasons)

| Suspect (from the task list) | Verdict |
|---|---|
| Fixed-position corruption | Ruled out by bench (card A passes identical code); all buffer/index math is size-based, never content-based — except `read_atr`'s skip-leading-0x00 (`smartcard.rs:566-568`), ATR-only and both ATRs start 0x3B |
| I-block chaining / reassembly boundaries at these sizes | No chaining occurs: 30 INF ≤ min(IFSC 254, IFSD 254); 34-byte block ≪ 271-byte buffers; single USB packet (`handler.rs:96`, `smartcard_common.rs:34-44`) |
| R-block retransmission logic / S(WTX) handling in firmware | There is none in the pipe — correctly delegated to libccid (reader-role doctrine, AGENTS.md 2026-10-12); `t1_engine.rs` defects (no WTX BWT extension, no resync escalation) are **dead code, zero callers** |
| LRC validation on receive | Not ours to do (pipe); host does it — see the LRC wall |
| `is_pps_request` XfrBlock misdetection | Requires len 3..=5, byte0=0xFF (`crates/ccid-core/src/lib.rs:134-145`); T=1 blocks start NAD=0x00 and are 17/34 B here |
| CCID descriptors altering host behavior | dwFeatures=0x000100BA TPDU + all-auto bits (reader does PPS/clock/baud itself → libccid sends none of those), dwMaxCCIDMessageLength=270 ≥ our max 268-byte message, AUTO_IFSD correctly absent so libccid negotiates IFSD through the pipe — all reference-locked (`device_profile.rs:447-473`, tests 600-695) |
| pcscd SetParameters touching card state mid-session | SetParameters only validates + echoes + `set_protocol()` (`handler.rs:802-879`, `ccid_core.rs:228-231`) — idempotent for an already-T=1 USART; nothing is sent to the card |
| DWT watchdog math | u64 intermediate, saturating (`dwt_watchdog.rs:18-25`) — timeouts fire when intended |
| SmartcardWrapper self-healing | Cold-resets only after 3 consecutive *errors* (`main.rs:220-245`); a crypto-clean session has none; and `transmit_raw`'s `Ok(0)` paths reset the streak to zero |

---

## 3. Decisive bench experiment (≤1 auth attempt; 2 of 3 stages consume zero budget)

Card B retry budget is at the line — **stages 1 and 2 cost zero auth attempts**; only stage 3
spends the one gp -l.

### Stage 1 — zero-attempt instrumented run (defmt RTT + Escape 0xD0), card B, no auth

1. Acquire `labgrid-client -p stm32-ccid acquire`. Flash the current release build; attach
   `probe-rs run` with `DEFMT_LOG=debug` (RTT). Note: the existing cycle stamps
   `"xfr: tx=…c frx=…c tot=…c rx=…"` (`smartcard.rs:690-696`) print **first-response-byte
   turnaround in DWT cycles /168 = µs** — that is the H1 latency histogram, already shipped.
2. Run a **read-only** card-B session through pcscd: connect → ATR → SELECT ISD → GET DATA
   (CPLC) — the differential-suite style probe (`tests/hardware/nfc/` machinery), which
   returns card-controlled *deterministic* bytes (including more `0x27`-class values) with
   **no secure-channel attempt**.
3. Watch for the H1/H2/H3 signatures: `Rx timeout 500ms`, `USART err SR=0x…` (PE=bit0,
   ORE=bit3), `prepare_rx: drained N` (never prints in this path — its absence confirms no
   drain), and `frx=` first-byte latencies near/over 500 ms (8.4e6 cycles).
4. Escape 0xD0 before/after (`ccid_core.rs:412-427`): `apdu_rx_count == apdu_tx_count` with
   clean RTT log ⇒ our layer relayed everything ⇒ strengthens H1's "mis-association" variant
   over byte corruption.
   **Verdicts:** any 500 ms `Rx timeout` during a card-B exchange ⇒ **H1 confirmed, fix
   immediately, stage 3 optional.** Any `USART err` with PE on a specific byte value ⇒ **H2/H5
   confirmed** (record the byte; expect the `0x27`-pattern class). Clean RTT at stage 1 ⇒ go
   to stage 2.

### Stage 2 — read-only Cherry-vs-ACR byte diff (still zero auth)

Same read-only APDU set through the ACR1252 (same card, same session if possible). Diff the
R-APDUs byte-for-byte: CPLC data is deterministic, so **any** single-byte difference isolates
the corrupted byte and its position without touching the ISD keys. (Both stages use the
`flock /run/lock/bench-pcscd.lock` pcscd serialization rule.)

### Stage 3 — the one gp attempt, with the wire tapped

```bash
flock /run/lock/bench-pcscd.lock systemctl stop pcscd pcscd.socket
LIBCCID_DEBUG_LEVEL=7 pcscd -f -d 2>&1 | tee /tmp/opencode/cherry-cardB-pcscd.log   # terminal 1
probe-rs run --chip STM32F469NI target/.../ccid-firmware   # DEFMT_LOG=debug, terminal 2 (RTT tap)
gp -l -v -d <AUTH triple> -r <Cherry reader index resolved fresh>                   # terminal 3
flock /run/lock/bench-pcscd.lock systemctl start pcscd pcscd.socket
```

* **In the libccid trace, diff these positions of the INITIALIZE UPDATE R-APDU** (gp `-d`
  prints the parsed fields — use its names as coordinates): KDD[0..10] (esp. **offset 6**:
  expect `27` — any deviation = the smoking byte), keyver[10], SCP[11..13], the 2-byte
  sequence counter, then the 8-byte card challenge and 8-byte card cryptogram (random —
  ignore content, but check **length**: a 27- or 29-byte response = drop/dup), SW (must be
  `90 00`). Against the ACR capture of the same card.
* **The discriminator is the T=1 activity *around* the response:**
  * Any R-block / S(RESYNC) / retransmission in the trace with the auth failing afterwards ⇒
    **comm-class confirmed → H1/H2/H3 family** (and the "cryptogram mismatch" wording was the
    tail of a recovered-but-misassociated exchange, or a paraphrase).
  * A byte diff in KDD/SCP/counter in a block libccid accepted **with valid LRC and no
    R-blocks** ⇒ **LRC-invisible corruption → H6 escalation**: Escape 0xD1 snapshot during a
    read-only repro, on-device XOR logging of every card block (see patch), and audit the
    vendored `synopsys-usb-otg` patch.

### What instrumented firmware evidence would prove the mechanism without further auth attempts

Already-shipped observables (no code change): the `USART err SR=/byte=` warns, the
`Rx timeout …ms` warns, the `xfr: frx=` first-byte cycle stamps, 0xD0 counters. Two small
additions (post-diagnosis, behavior-neutral): (a) PE/ORE per-exchange counters surfaced in the
0xD0 28-byte diagnostic; (b) an on-device LRC/length assert of each received card block
(count-only, never act) — either turns the next *read-only* session into a permanent monitor.

---

## 4. Minimal patch sketch (pipe-doctrine compliant — electrical/receive layer only, no T=1 logic)

All in `firmware/ccid-firmware/src/smartcard.rs`, `transmit_raw` T=1 branch (lines 648-698):

```rust
// (1) Drain stale RX (echoes, orphaned tails, ORE residue) before every TX.
//     prepare_rx() already exists (line 795) but is only wired to the dead t1_engine.
self.prepare_rx();

// (2) First-byte timeout must cover the card's BWT, not a hardcoded 500 ms.
//     We echo BWI=4 to the host (no TB3 in ATR → parser default), i.e. we tell
//     libccid the card may take ~1.38 s; the reader must be at least as patient.
//     Derive from atr_params.bwi (or clamp: max(500, bwt_ms) with the 5 s
//     procedure_timeout cap). Inter-byte stays generous (CWT≈1 ms at CWI default;
//     50 ms is 47× margin — keep, it is not the failure mode).
let first_byte_ms = bwt_ms_from(self.bwi);            // 11 + 2^bwi*960 etu → ms, + margin

// (3) Never report a timeout as Ok(short) — an empty/truncated "success" DataBlock
//     is a contract violation at TPDU level (host engine mis-reads it, then races
//     the late card block). Return Err → CCID CMD_ABORTED/ICC_MUTE shape:
Err(SmartcardError::Timeout) => return Err(SmartcardError::Timeout),
//     (the mid-INF `break` at line 686 becomes a ProtocolError for the same reason)

// (4) In receive_byte_timeout (line 724): count PE and ORE into Diagnostics
//     (0xD0 extension); on ORE specifically return Err(ProtocolError) — after an
//     overrun the stream is shredded; feeding the stale DR byte as a prologue is
//     strictly worse than failing the exchange.
```

(1) kills the stale-byte contamination seed; (2)+(3) remove the race that orphans card
blocks; (4) converts silent shredding into an observable, honest error. None of this inspects
T=1 content — the host keeps owning the protocol (reader-role doctrine; #132 is explicitly
*not* license for on-device T=1 logic). Add host tests pinning: timeout returns Err, drain
called before TX, ORE rejected — mirroring the reference behavior of any commercial TPDU pipe.

**Residual risk / honest unknowns:** H1's mis-association variant predicts the pcscd trace
shows R-block/retry activity around INIT UPDATE; if stage 1+3 come back *completely* clean
with a byte diff, the remaining suspect is the vendored USB driver — a materially different
investigation. The 500 ms-exceeded-by-card-B premise is plausible but unproven until the
`frx=` stamps are read off card B; treat H2 (0x27-pattern marginal sampling) as the co-favorite
if stage 1 shows PE warns instead.

## 5. Issue-tracker actions

* Encode H1/H2 verdicts on #132 as a checklist (per the issue-truth rule); cross-reference
  commit `13b17c8` as the timeout provenance and `b1e685a` as the pipe decision.
* The AGENTS.md "known gaps" note for the Cherry/STM32 row should link this report path.

---

# Appendix A — Reference Audit (2026-10-12, follow-up)

Audit of the §2 hypotheses against references: the osmo-ccid-firmware submodule (the
"sysmocon"/sysmoOCTSIM project this repo studied), the **tracked libccid source in
`reference/CCID/`** (including the exact openct T=1 engine our reader faces,
`src/openct/proto-t1.c`), ST's official F4 smartcard HAL header, and the specquotes/
greatspectations corpus. Verdicts below **change two hypothesis ranks** and turn the
remaining benchwork from trial-and-error into confirmation.

## A.1 What the host actually does with each response shape we emit — now source-proven

`CCID_Receive` (`reference/CCID/src/commands.c:1546-1639`) maps every RDR_to_PC outcome:

| Our firmware emits | libccid does | T=1 engine does | Reference |
|---|---|---|---|
| Success DataBlock, N bytes | copies N bytes | `t1_xcv` truncates to `block[2]+3+rc_bytes` — **trailing stale bytes are harmlessly cut** | proto-t1.c (`t1_xcv` tail: `m = block[2] + 3 + t1->rc_bytes; if (m < n) n = m;`) |
| **Success DataBlock, 0 bytes — our `Ok(0)` on 500 ms timeout (`smartcard.rs:674`)** | `length-10 == dw2i(cmd,1)` holds; `*rx_length = 0`, returns IFD_SUCCESS | `sdata` still holds the block the **host sent**; NAD/LEN checks pass on it; `t1_verify_checksum(…, 0)` fails → **"checksum failed" → R(EDC_ERROR) retransmission of the I-block** | commands.c:1611-1626 + proto-t1.c checksum-fail arm |
| Failed SlotStatus bError=0xEF (our `CMD_ABORTED` Err arm) | synthesizes SW `64 01`, returns IFD_SUCCESS | NAD=0x64 ≠ 0 → R(OTHER_ERROR) ladder | commands.c:1568-1574 |
| Failed bError=0xF0 (timeout) | synthesizes SW `64 00` | same ladder | commands.c:1576-1582 |
| **Failed bError=0xFD (parity)** | returns `IFD_PARITY_ERROR` | `t1_xcv` → −2 → "Parity error" → R(EDC) per Rule 7.4.2 | commands.c:1584-1585, proto-t1.c (`if (-2 == n)`) |
| DataBlock with `CCID_TIME_EXTENSION` in bStatus | multiplies read timeout, re-reads | — | commands.c:1598-1608 |

**Consequences for §2:**

* **H1's retry leg is proven.** Our `Ok(0)` empty "success" makes libccid retransmit the
  I-block through us → the card sees it twice while its first answer races → after 3
  retries S(RESYNC) resets the card's T=1 state. Combined with §1's no-pre-TX-drain and
  ORE-tolerant read, the wrong-exchange block delivery (the only LRC-wall-crossing path)
  is mechanically complete. **H1 stays #1, now with host-side source evidence.**
* **H3's downstream is halved (in our favor):** `t1_xcv` cuts any trailing stale bytes
  after the declared LEN — stale *tails* can't corrupt the next block at the host. Only
  the *leading* stale byte (shifted prologue) remains dangerous — exactly what the missing
  `prepare_rx()` before TX would fix.
* **The LRC wall itself is now citable line-by-line**: `t1_verify_checksum` fail →
  R(EDC) → Rule 7.4.2 (retries exhausted) → Rule 6.4 (3 resyncs) → `t1->state = DEAD`
  (proto-t1.c, all four error arms).
* **Reference contract we violate — parity reporting**: a TPDU-level reader is *expected*
  to be able to answer a character parity error with bError=0xFD, which the engine turns
  into the sanctioned R(EDC) retry. Our `receive_byte_timeout` swallows PE (warn-only) —
  the corruption rides into the block instead. Not wrong per ISO, but the reference
  behavior exists, is cheap, and makes H2 observable at the CCID layer.

## A.2 osmo-ccid-firmware (sysmoOCTSIM) — the reference reader contract

osmo is a **character-level** exchange reader (dwFeatures low byte 0x00) with a **T=0-only**
ISO FSM (`docs/T1_BEST_PRACTICES.md` §4) — so it has no T=1 timing tables to copy. What it
does have is the **timeout/error contract** our firmware violates:

* **`ccid_slot_fsm.c:215-227` — waiting-time expiry → deactivate the card + respond
  `CCID_CMD_STATUS_FAILED`/`CCID_ERR_ICC_MUTE` DataBlock.** Never a short or empty success.
  This is the reference shape for "the card was too slow": a *failure*, which through
  libccid maps into the engine's sanctioned retry path. Our `Ok(0)`/`Ok(short)` is the
  anti-pattern.
* **`sysmoOCTSIM/config/hpl_sercom_config.h`** — the SAMD51 SERCOM ISO7816 config:
  even parity (FORM=7), **`CONF_SERCOM_0_USART_INACK 0x0` — "NACK is transmitted when a
  parity error is received"**, DSNACK=0, **two stop bits** (per `atmel_start_config.atstart`).
  The reference hardware **keeps the ISO character-level retry safety net in hardware for
  every protocol**. Our F469 disables it for T=1 (NACKEN=0, GT=1) — legal per ISO 7816-3
  (T=1 relies on block EDC), but it is a real robustness delta vs the reference, and it is
  the enabler that lets content-dependent character corruption reach the block layer.

## A.3 ST's own F4 HAL — H5 upgraded from "marginal" to "citable violation"

From `STMicroelectronics/stm32f4xx-hal-driver` `Inc/stm32f4xx_hal_smartcard.h` (ST's
RM0090-derived contract; **not yet tracked in this repo**):

```c
#define SMARTCARD_STOPBITS_0_5              ((uint32_t)USART_CR2_STOP_0)
#define SMARTCARD_STOPBITS_1_5              ((uint32_t)(USART_CR2_STOP_0 | USART_CR2_STOP_1))
...
#define IS_SMARTCARD_STOPBITS(STOPBITS)     (((STOPBITS) == SMARTCARD_STOPBITS_0_5) || \
                                             ((STOPBITS) == SMARTCARD_STOPBITS_1_5))
```

**On the STM32F4 USART, smartcard mode supports ONLY 0.5 or 1.5 stop bits — ST's HAL
asserts it.** Our ATR-time config is legal (CR2=0x3900 → STOP=1.5), but the T=1 switch at
`smartcard.rs:414-419` **clears CR2 bits 13:12 → STOP=`00` = 1 stop bit — outside the
smartcard-mode contract.** It has "worked" with every fast/new card tested (ComSign,
card A) — but it shaves the character framing to its margin, which is precisely the regime
where per-bit-pattern sampling (§2-H2's `0x27` 3-bit-LOW-run class) becomes
card-input-stage-dependent, i.e. **deterministic per card**. H5/H2 move up as the co-lead
electrical hypothesis behind H1's timing trigger.

**Patch addition (reference-correct):** in the T=1 branch of `power_on`, keep
STOP=1.5 (do **not** clear CR2 13:12) and express the reduced guard time purely through
GTPR (and note: this ATR has **no TC1** → ISO default guard N=2 — the current GT=1 is
itself more aggressive than the card was ever told).

## A.4 The reasoning framework — deciding by arithmetic, not bench iterations

All numbers from the references, at our actual wire (card clock 4.2 MHz, Fi=372/Di=1 →
ETU = 88.57 µs, 11290 baud; ATR has TA3 only → no TB3/TC1):

| Window | Formula & authority | Legal value | Our setting | Verdict |
|---|---|---|---|---|
| CWT | `(2^CWI + 11) etu`, CWI=0 default (no TB3) [ISO 7816-3 §11.6.4 via T1 doc §2.4] | **1.06 ms** | 50 ms | 47× margin ✓ |
| BWT (ISO default) | `(11 + 2^BWI·960) etu`, BWI=0 | **86.0 ms** | 500 ms | 5.8× ✓ |
| **BWT as WE report it** | BWI=4 (`parse_atr` default, echoed in GetParameters byte 3 = 0x4D → host readTimeout ≥ 1.36 s; libccid floors ≥3 s — NEWS 1.5.0) | **1.361 s** | 500 ms | **✗ reader 2.7× less patient than the parameters it advertises — a self-inconsistency provable without hardware** |
| WTX ceiling | m ≤ 59 × BWT, granted-then-multiplied (proto-t1.c `t1_xcv`) | ≤ 80 s | host-owned | ✓ pipe-compatible |
| BGT | 22 etu | 1.95 ms | DWT-polled µs | ✓ |
| Stop bits (F4 smartcard) | 0.5 or 1.5 only — `IS_SMARTCARD_STOPBITS` | — | **1.0** | ✗ violation (A.3) |
| Character guard | N=2 default (no TC1) enables NACK retry; osmo keeps it even for its T=0 FSM | N=2 | GT=1 + NACKEN=0 | ✗ off-reference (A.2) |

And the determinism argument, closed by the audit:

1. The bench failure is deterministic per card (2/2, two keysets).
2. The only per-card-deterministic quantities are **fixed response content** (KDD serial)
   and **per-mask latency**; random challenge/cryptogram bytes cannot drive a 2/2
   deterministic failure (p ≈ 0.3%).
3. Content corruption is **LRC-walled** — now proven from the tracked openct source
   (A.1) — unless it recurs identically through the R(EDC) retransmission ladder and ends
   in a mis-associated valid block.
4. Every enabler of that path is a **reference-prohibited behavior we ship**: Ok-on-timeout
   (osmo answers ICC_MUTE), no pre-TX drain (echo discipline is repo-wide elsewhere),
   PE-swallowing (reference answers 0xFD), out-of-spec stop bits (ST's own assert).

⇒ The hypotheses were never independent: they form **one causal chain**, and the bench
experiment (§3) now only establishes **which link fires first on card B**.

## A.5 Greatspectations additions (all from tracked files unless noted)

Add to `specquotes.toml` + `// MARKER:` comments at the code sites:

| Source | File (tracked?) | Verbatim quotes to lock | Code sites to annotate |
|---|---|---|---|
| `OPENCT` | `reference/CCID/src/openct/proto-t1.c` ✓ | checksum-fail → R(EDC) ladder w/ Rule 7.4.2/7.2 cites; `m = block[2] + 3 + t1->rc_bytes;` truncation; WTX timeout multiply in `t1_xcv`; `t1_set_defaults` (`ifsc = 32`, `retries = 3`) | `smartcard.rs` `transmit_raw` (why trailing bytes are safe, why Ok(0) ⇒ retransmit); `t1_engine.rs` |
| `LIBCCID` | `reference/CCID/src/commands.c` ✓ (source exists, new quotes) | `case 0xFD: /* Parity error during exchange */ return IFD_PARITY_ERROR;` — and the full 0xEF/0xF0/0xFD/0xFE switch; `if (length-10 != dw2i(cmd, 1))` | `ccid_core.rs` error-shape arms; the planned 0xFD arm |
| `OSMO_FSM` | `reference/osmo-ccid-firmware/ccid_common/ccid_slot_fsm.c` ✓ | `case ISO7816_E_WTIME_EXP: … resp = ccid_gen_data_block(cs, ss->seq, CCID_CMD_STATUS_FAILED, CCID_ERR_ICC_MUTE, 0, 0);` | the timeout→Err fix in `transmit_raw` |
| `OSMO_UART` | `reference/osmo-ccid-firmware/sysmoOCTSIM/config/hpl_sercom_config.h` ✓ | `#define CONF_SERCOM_0_USART_INACK 0x0` + the `<0x0=> NACK is transmitted when a parity error is received` option text | `smartcard.rs` NACK/GT config |
| `ST_HAL` | **not tracked — vendor it** (like `reference/CCID`, e.g. `reference/ST/stm32f4xx-hal-driver/Inc/stm32f4xx_hal_smartcard.h`) | `#define IS_SMARTCARD_STOPBITS(STOPBITS) (((STOPBITS) == SMARTCARD_STOPBITS_0_5) \|\| ((STOPBITS) == SMARTCARD_STOPBITS_1_5))` | the T=1 stop-bit fix + a unit test asserting the CR2 value written |
| **Gap: T=1 timing formulas** | `reference/CCID/src/ifdhandler.c` is **absent** from the vendored copy (it computes the host readTimeout from ATR TB3/BWI — the missing BWT authority) | readTimeout computation | the planned `bwt_ms_from(bwi)` helper |

Plus the `docs/greatspectations` unit tests to pin once the patch lands: BWT/CWT
derivation from `atr_params` (86 ms / 1.06 ms at defaults; 1.361 s at BWI=4), the CR2
stop-bit value for T=1, and timeout→`Err` (never `Ok(short)`) at the `transmit_raw`
boundary.

## A.6 Audit deltas to §2 ranking

| Hypothesis | Before | After audit |
|---|---|---|
| H1 timeout/orphan chain | #1 (code-only reasoning) | **#1, source-proven retry leg** (A.1) |
| H2 content-dependent corruption | #2 | **#1b (co-lead)** — H5's out-of-spec stop bits is now its citable physical mechanism |
| H3 stale-byte contamination | #3 | #3, narrowed to *leading*-byte contamination only (A.1) |
| H4 SetDataRate mid-session | #4 | #4 unchanged — libccid trigger conditions in `ifdhandler.c` are untracked; keep the capture check |
| H5 stop-bit/GT config | #5 | **folded into H2** as the electrical mechanism (A.3) — violation confirmed |
| H6 in-buffer permutation | #6 residual | #6, further weakened: `t1_xcv` truncation removes the tail-stitch variants |
