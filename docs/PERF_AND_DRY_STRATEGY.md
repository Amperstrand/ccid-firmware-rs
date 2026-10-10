# Performance & DRY Strategy — ccid-firmware-rs

**Created**: 2026-10-10, from the live bench measurement session + osmo-ccid-firmware
reference study. This is the working strategy for getting all three of our readers
on par with the commercial references, and for consolidating the duplicated CCID
handling into one shared core. Update it as lanes land.

## 1. Measured baseline (2026-10-10, `tests/hardware/perf/benchmark_readers.py`, n=300)

| Reader | Status | mean/APDU | p99 | connect | Blocker |
|---|---|---|---|---|---|
| ACR1252 (reference) | full GPG | 8.8 ms | 8.9 | 39 ms | — |
| CardMan 3121 (reference) | full | 10.9 ms | 11.0 | 39 ms | — |
| **Cherry/STM32F469 (ours)** | works | **95.3 ms** | 95.4 | **1507 ms** | fixed per-exchange cost (agent: stm32-perf) |
| **GemPCTwin/m5stick (ours)** | works, cardless | ~10–30 ms (T=1 relay) | — | ~300 ms | card off coil (physical); CAP-upload relay latency |
| **Nucula (ours)** | enumerates | **5.1 ms** p50 | — | — | **2.0 s stalls @ 5.7 s cadence** → pcscd retirement (agent: nucula-rearm); cardless |

Interpretation:
- The STM32 gap is a *systematic fixed cost* (p99−p50 = 0.09 ms — it is a
  constant, not jitter). Fix belongs in one place, measurable to the ms.
- The nucula p50 already beats the ACR — only the periodic stall and the
  missing card keep it off-par. The stall is deterministic (exactly ~2000 ms,
  exactly ~5.7 s apart) → a synchronous blocking call on the serve path.
- The m5stick T=1 relay is latency-competitive for command-sized exchanges;
  the CAP-upload stall (~470 chained blocks) is the one known slow path.

## 2. The compare/contrast method (already built — use it)

1. **`tests/hardware/perf/benchmark_readers.py`** — the measuring stick:
   same APDU, same card where possible, percentile stats per reader. Run
   before/after every perf-touching change; the table above is the format.
2. **`tests/hardware/nfc/test_differential.py`** — correctness comparison:
   one card through every reader, ATR + AID + read-only APDU agreement
   (issue #87's instrument). Needs cards stably coupled to run.
3. **`tests/hardware/nfc/conformance_battery.py`** — fuzz/soak agreement
   between OUR readers (no card needed).
4. **Refactor gate**: any serving-path consolidation must keep the battery
   byte-identical (the #90 precedent) and the perf table non-regressing.

## 3. DRY targets (evidence-based)

### 3.1 Two parallel CCID message handlers (the big one)

- STM32: `firmware/ccid-firmware/src/ccid_core.rs` — `CcidMessageHandler`,
  1206 lines. Has: PIN verify/modify (Secure), T=0/T=1 contact.
- ESP32: `firmware/esp32-ccid/src/ccid_handler.rs` — `CcidHandler`, 1282
  lines. Has: T=1 endpoint routing (t1.rs), Escape 0xD1 snapshot,
  fuzz-tested 5300 rounds (#92), CLA-rewrite relay semantics (#50).
- Both implement the same CCID Rev 1.1 §6 dispatch: slot lifecycle,
  bStatus assembly, escape diagnostics, parameter echo. Divergences are
  historical, not fundamental.

**Direction**: extract one handler into `crates/ccid-core` behind the
existing card-driver trait (the crate already exists and is shared).
Port order: move the ESP32 handler (younger, fuzz-hardened) → teach it
the STM32's PIN/secure surface → point the STM32 USB transport at it.
Gate: battery byte-identical + perf non-regressing + HIL 7/7.

### 3.2 What osmo-ccid-firmware teaches (reference study)

Their layout: one `ccid_common/` — `iso7816_3.c` (the single ETU/guard/
waiting-time translation), `ccid_slot_fsm.c` (one formal slot lifecycle
FSM), `ccid_proto.c` + `ccid_device.c` (protocol + device glue) — with
thin board frontends. The lesson is not the files but the *seams*:
- **timing layer**: one module owns ETU/guard/waiting-time math (our
  equivalents are scattered: smartcard.rs on STM32, driver-side waits
  on ESP32).
- **slot FSM**: one explicit state machine for ICC lifecycle (ours is
  implicit enum-toggling inside two handlers — the unification in 3.1
  should surface it as a real FSM).
- **board frontends stay thin** — our equivalent after #90 for the
  ESP32 UART mains; the STM32 main loop is the remaining thick one
  (and the 95 ms hunt lives exactly there).

### 3.3 Diagnostics + escape surface

- Escape 0xD0 (28-byte counters) exists on BOTH handlers; the struct
  lives in ccid-core (shared) but the dispatch is duplicated. Unifies
  with 3.1.
- Escape 0xD1 (snapshot) is ESP32-only; STM32 has no flash-coredump —
  port later, not a blocker.

### 3.4 t1.rs (ESP32-only today)

ISO 7816-3 §11 endpoint serving the libccidtwin serial path. The STM32
is a USB CCID device (Short-APDU level) and does not need it. If the
STM32 ever exposes TPDU exchange, t1.rs is transport-agnostic (pure
block↔APDU logic) and moves to ccid-core unchanged.

## 4. Sequencing (lanes)

| # | Lane | State | Unblock |
|---|---|---|---|
| 1 | nucula re-arm stall fix | **agent running** (nucula-rearm, wG) | nucula pcscd survival |
| 2 | STM32 95 ms root-cause | **agent running** (stm32-perf, wH) | STM32 on-par |
| 3 | card placement (m5stick + nucula coils) | **human** — both verified cardless via known-good + power-cycle | #87 differential, GPG-through-our-readers |
| 4 | differential rerun (#87 close) | blocked on 3 | card-level correctness across readers |
| 5 | handler unification (3.1) | blocked on 1+2 landing + battery gate | the big DRY |
| 6 | m5stick CAP-upload perf | blocked on 3 (need a card to test) | install-through-our-reader |

## 5. Card-safety rules (standing)

- GP keys: default `40..4F` or the AUTH triple from
  `tests/soak/soak_02_globalplatform.py` — nothing else; wrong keys
  consume the ISD retry counter (brick risk).
- On-card key generation is safe (SmartPGP); key *import* is the buggy
  path — always answer NO to GnuPG's backup/move prompt.
- Bench cards move without notice — verify coupling (present flag via
  Escape 0xD0) before any card-level conclusion; a cardless reader is
  `present=0` on a known-good build after a hard reset, not a firmware
  bug (today's 4-step proof: current build, 46cb50c, USB power-cycle,
  port-identity check).
