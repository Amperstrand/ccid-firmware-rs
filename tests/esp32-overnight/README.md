# esp32-overnight — Track D (ccid window)

Overnight harness lane for the ccid window of the dual-role audit (plan todo
13, `.omo/plans/overnight-ccid-bolty-audit.md`). Runs after the mid-night
role switch flashes the stick with `esp32-ccid`: soak tests against the
firmware reader, Escape 0xD0 diagnostics telemetry, and an ATR stability
loop. No hardware is touched by anything in this directory during tests —
pyscard is stubbed, readers are fakes.

## Files

| File | Purpose |
|---|---|
| `track_d.py` | The lane: key-audit gate, reader gate, soak adapter, telemetry, ATR loop, `run(ctx)` / `build_lane()` / `register(ctx)` |
| `soak_allowlist.json` | Reviewed key-audit verdicts (per test: verdict, key risk, evidence). Regenerate with `track_d.py --audit`, review, commit |
| `tests/test_track_d.py` | Offline pytest suite (mocked pyscard) |

## Soak key-audit gate (load-bearing)

`tests/soak/` was written for a JR3180/JavaCard rig; several of its suites
authenticate with hardcoded/JavaCard key material. Tonight's NTAG424 cards
run deterministic keys — a wrong-key APDU burns card auth budget
(SeqFailCtr/TotFailCtr, AN12196 §7.4), violating the proven-safe envelope.

Before any soak test runs, `SoakGate` requires BOTH of these to say
INCLUDED, else it raises `SoakGateError` and the test is refused:

1. the reviewed verdict in `soak_allowlist.json`, and
2. a fresh static AST audit of `tests/soak/*.py` + `soaklib.py` (guards
   against source drift since the review).

The audit resolves every APDU each registered test sends (module constants,
`bytes.fromhex`, `bytes([...])` with constant INS, for-loop/tuple-unpack
indirections) and EXCLUDES, fail-closed:

- any test sending an auth-family INS (`20` VERIFY, `22` MSE, `82`/`88`
  AUTHENTICATE, `84` GET CHALLENGE, `AF` NTAG424 AUTHENTICATE),
- any module importing auth tooling (pysatochip, gp.jar, gpg, opensc-tools)
  or defining PIN/KEY-named constants,
- any APDU construction it cannot statically resolve.

Verdicts: suites 01 (6/6), 09 (7/7), 10 (14/14) and the suite-08 100/255-byte
boundary subset (2 tests) are INCLUDED — provably key-free SELECT/ATR/
transport ops. Suites 02–07 are EXCLUDED (foreign key material: GP SCP,
SatoChip PIN "1234", PIV PIN "12345678", gpg/opensc tooling). Suite 11 and
the rest of suite 08 are EXCLUDED by scope. Regenerate + review after any
`tests/soak/` change:

```bash
python3 tests/esp32-overnight/track_d.py --audit > /tmp/audit.json
# review, then: cp /tmp/audit.json tests/esp32-overnight/soak_allowlist.json
```

Unlike the daytime rig, this adapter never auto-files GitHub issues and
never FAILs on response-content differences between the two readers — the
ACR1252 and the firmware reader hold different cards tonight.

## Telemetry + reader notes

- Escape 0xD0 goes through `SCardControl`
  (`FEATURE_CCID_ESC_COMMAND` from `getFeatureRequest`). If the pcscd
  serial stack does not advertise it (libccid gates ESC behind
  `ifdDriverOptions=0x0001`), the lane records an honest
  `escape_supported=false` SKIP row — the raw-serial fallback is todo 14.
- Diagnostics payload = 28 bytes, 7 LE u32 words
  (`crates/ccid-core/src/diagnostics.rs`): apdu_tx, apdu_rx, nak, error,
  reinit, card_present, uptime_ticks. Asserted: counters non-decreasing,
  reinit delta 0 across the window, uptime monotonic.
- ACR1252 exposes two pcscd entries (PICC `00 00`, SAM `01 00`); reader
  picks prefer the lowest channel and avoid the SAM entry.
- Round-2 amendment: pcscd maintenance pauses this lane. Use the module's
  `pause()`/`resume()` (or the orchestrator's `needs_pcscd` lane pause) —
  every reader operation re-checks the gate first.

## Commands

```bash
python3 -m pytest tests/esp32-overnight/tests -q   # offline tests
python3 tests/esp32-overnight/track_d.py --selftest
ruff check tests/esp32-overnight/
```

Lane wiring (bolty-rs `tools/hil/overnight/overnight.py`): the loader
imports `track_d` by name and calls `build_lane()` →
`LaneSpec(name="track_d_soak", window="window2", needs_pcscd=True)`.
Orchestrators that prefer a one-shot call may use `register(ctx)`; both are
duck-typed against the `PhaseContext` API.
