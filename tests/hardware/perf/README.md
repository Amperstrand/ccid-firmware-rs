# Smartcard reader performance suite

Parameterized, unattended pytest benchmarks for every card-bearing
reader pcscd lists — the ACR1252 reference anchors each comparison, the
Nucula vpcd readers (WiFi transport, ~2 s/APDU today) are the targets.
Successor to `benchmark_readers.py` (same directory, still the quick
one-shot stick) and `tests/hardware/nfc/perf_ab.py`; results land in an
append-only ledger and `report.py` renders the readers × metrics table.

Everything here follows `docs/VPCD_WIFI_RUNBOOK.md` (reader names,
real-connect presence, ~2 s/APDU budget) and the repo card-safety
doctrine (read-only APDUs only).

## Running

```sh
# 1. preview without touching pcscd (busy-bench safe; also: --list)
python3 -m pytest tests/hardware/perf/ --dry-run

# 2. real run (bench opt-in gate; every card-bearing reader discovered)
NUCULA_PERF=1 python3 -m pytest tests/hardware/perf/ -v

# 2b. narrow to specific readers (repeatable; substring of the pcscd
#     name, a class id, or a class match key)
NUCULA_PERF=1 python3 -m pytest tests/hardware/perf/ -v --reader "Nucula VPCD 00 00" --reader PICC

# 3. comparison table (latest run per reader; ACR1252 pinned as reference)
python3 tests/hardware/perf/report.py
python3 tests/hardware/perf/report.py --out perf-report.md   # or to a file
python3 tests/hardware/perf/report.py --all                  # full history
```

Collection works with no hardware, no pyscard and no gate:
`python3 -m pytest tests/hardware/perf/ --collect-only` collects the
3 tests × 3 reader classes without a single pcscd call. Without
`NUCULA_PERF=1` every test skips.

## Tests

| file | metric(s) | what it measures |
|---|---|---|
| `test_atr_latency.py` | `atr_latency_ms` | wall time from initiating `SCardConnect` to holding the ATR (reconnect per sample; disconnect only *between* samples) |
| `test_apdu_rtt.py` | `apdu_rtt_select_ms`, `apdu_rtt_get_data_ms` | per-APDU round trip on one long-lived connection |
| `test_throughput.py` | `stream_{small,large}_apdu_ms`, `throughput_{small,large}_apdus_per_sec`, `throughput_{small,large}_bytes_per_sec` | closed sequential stream of 6 B and 255 B command APDUs; rate + effective bytes/sec (cmd + resp incl. SW) |

Every metric reports **p50/p90/p99/max** from the raw samples; the two
throughput aggregates carry their value in `p50` (nulls elsewhere — an
aggregate has no distribution).

## Parameters

Iteration counts scale **per reader class** so the USB reference never
runs 10 000 iterations and vpcd never times out at 20 APDUs: the
single `PARAMS` dict in `perflib.py` holds the per-class numbers.

| reader class | match (all substrings in pcscd name) | transport | iterations | warmup | atr_samples |
|---|---|---|---:|---:|---:|
| `acr1252` | `ACR1252` + `PICC` (contactless interface, not SAM) | `usb-ccid` | 50 | 3 | 10 |
| `nucula-vpcd` | `Nucula VPCD` (slots `00 00` nucula / `00 01` m5stick) | `wifi-vpcd` | 10 | 1 | 3 |
| `generic` | any other card-bearing reader (fallback) | guessed from name | 50 | 3 | 10 |

Overrides (win over the per-class defaults):

| knob | default | effect |
|---|---|---|
| `--reader SUBSTR` (repeatable) / env `NUCULA_PERF_READERS` (comma list) | all discovered | keep a reader when the value matches its pcscd name, its class id, or a class match key (case-insensitive) |
| env `NUCULA_PERF_ITERATIONS` | per class | overrides every class's stream/RTT iteration count |
| env `NUCULA_PERF_ATR_SAMPLES` | per class | overrides every class's ATR sample count |
| env `NUCULA_PERF_WARMUP` | per class | overrides every class's warmup-discarded sample count |
| env `NUCULA_PERF_FW_REV` | `null` | firmware revision stamped on every ledger row (not queryable through pcscd without reader-specific escapes that would pollute the stream) |
| env `NUCULA_PERF_LEDGER` | `results/perf-history.jsonl` | alternate ledger path |

Add a new reader class by adding one entry to
`PARAMS["reader_classes"]` — the fixture parametrization picks it up
automatically.

## Ledger

`results/perf-history.jsonl` (append-only JSONL, **gitignored**). One
line per recorded metric:

```json
{"ts": "2026-10-13T18:22:04+02:00", "reader": "ACS ACR1252 Dual Reader [ACS ACR1252 Dual Reader PICC] 01 00",
 "transport": "usb-ccid", "fw_rev": null, "metric": "apdu_rtt_select_ms",
 "n": 50, "p50": 8.912, "p90": 9.001, "p99": 9.244, "max": 9.31}
```

* `ts` — ISO-8601 local time (sorts lexicographically).
* `reader` — exact pcscd reader name.
* `transport` — guessed from the class/name (`usb-ccid`, `wifi-vpcd`,
  `serial-ccid`).
* `fw_rev` — `NUCULA_PERF_FW_REV` or `null`.
* `metric` — one of the table above (unit is the suffix).
* `n` — samples recorded (post-warmup).
* `p50/p90/p99/max` — nearest-rank percentiles (same estimator as
  `benchmark_readers.py`, so numbers stay comparable with the
  2026-10-10 table in `docs/PERF_AND_DRY_STRATEGY.md`). `null` for
  non-existent quantiles of aggregate metrics.

## Methodology

* **time.perf_counter** for all timings (monotonic, highest resolution).
* **Warmup discarded** — first samples after a fresh connect absorb
  pcscd/driver cold start and card ATR/PPS negotiation; count is a
  per-class parameter.
* **GC disabled inside timed regions** (collect-then-disable, restore
  after) — a CPython GC pass is a classic ms-scale outlier.
* **Percentiles, never bare means** — the tail is the regression
  signal; closed sequential streams mean no coordinated omission.
* **Presence by REAL SCardConnect** — the pcscd GetStatusChange cache
  lies after daemon restarts (runbook "Operational rules" #2).
* **One connection per test** — never power-cycled mid-sample; the ATR
  test's reconnect is the sample boundary by definition.

References researched 2026-10-13 (also cited in `perflib.py`):

* pyperf (PSF) — GC disabled during runs, first values skipped as
  warmup: https://pyperf.readthedocs.io/en/latest/cli.html,
  https://github.com/psf/pyperf
* Gil Tene, *How NOT to Measure Latency* (percentiles, coordinated
  omission): https://www.infoq.com/presentations/latency-user-perspective,
  https://www.scylladb.com/2021/04/22/on-coordinated-omission
* Python `time.perf_counter`:
  https://docs.python.org/3/library/time.html#time.perf_counter
* Python benchmarking practice (warmup, GC, perf_counter):
  https://superfastpython.com/python-benchmarking-best-practices
* pyscard user guide (connect/transmit model):
  https://pyscard.sourceforge.io/user-guide.html
* vsmartcard — vpcd upstream (no bench harness to borrow; wire format
  and slot model only): https://github.com/frankmorgner/vsmartcard
* In-repo predecessors: `tests/hardware/perf/benchmark_readers.py`,
  `tests/hardware/nfc/perf_ab.py`, `docs/VPCD_WIFI_RUNBOOK.md`,
  `docs/PERF_AND_DRY_STRATEGY.md`.

No dedicated open-source smartcard-reader *benchmark* standard exists
to copy (checked 2026-10-13); the statistical practice above is what
this suite borrows.

## APDU safety

Read-only, by construction (`perflib.assert_safe_apdus` trips on
anything that is not SELECT/GET DATA):

* `SELECT` OpenPGP AID `D276000124010304AFAF000000000000` — applet
  selection, no state change.
* `GET DATA 00CA004F00` — read of the application identifier.
* Throughput streams are SELECT-by-DF-name lookups (6 B and 255 B);
  unknown names answer `6A82` and change nothing. On vpcd the 255 B
  stream answers SW `64 00` above the documented ~245 B NCI
  single-packet cap (runbook "GP") — still a completed transport
  exchange, which is what the metric measures.

Never sent by this suite: VERIFY/UPDATE/WRITE, GP authenticate, GP
delete/install, STORE DATA, PIN operations. No power-cycling
mid-sample.

## Baseline 2026-10-13 — TODO (orchestrator: fill after the first real run)

> Run `NUCULA_PERF=1 python3 -m pytest tests/hardware/perf/ -v` when
> the bench is free, then paste `python3 tests/hardware/perf/report.py`
> output here and commit this README change (the ledger itself stays
> gitignored).

| reader | atr p50 | select p50 | select p99 | get_data p50 | small apdus/s | large bytes/s |
|---|---|---|---|---|---|---|
| ACR1252 (reference) | TODO | TODO | TODO | TODO | TODO | TODO |
| Nucula VPCD 00 00 | TODO | TODO | TODO | TODO | TODO | TODO |
| Nucula VPCD 00 01 | TODO | TODO | TODO | TODO | TODO | TODO |

Historical anchor (2026-10-10, `benchmark_readers.py`, n=300,
`docs/PERF_AND_DRY_STRATEGY.md`): ACR1252 mean 8.8 ms/APDU, p99 8.9 ms,
connect 39 ms. vpcd expectation from the runbook: ~2 s/APDU.
