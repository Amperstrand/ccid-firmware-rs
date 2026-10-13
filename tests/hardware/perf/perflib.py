"""Shared library for the smartcard-reader performance suite.

Uniquely-named module (NOT conftest) so test modules can import it when
several hardware suites are collected together — same doctrine as
tests/hardware/nfc/cardlib.py and tests/hardware/labgrid/bench.py.
pyscard is imported lazily inside the functions that touch pcscd: this
module loads during pytest collection, which must also work in the CI
minimal env (pytest + stdlib only).

Methodology (researched 2026-10-13 — full list with links in README.md):

* Every timing uses time.perf_counter(): monotonic, highest-resolution
  clock on CPython, unaffected by wall-clock changes
  (https://docs.python.org/3/library/time.html#time.perf_counter).
* First-connection warmup is discarded: the first exchange after a
  fresh SCardConnect absorbs one-time costs (pcscd cold start, CCID
  driver init, card ATR/PPS negotiation). pyperf applies the same
  rule (skip first values as warmup): https://pyperf.readthedocs.io/
  en/latest/cli.html — this suite makes it a per-class parameter
  instead of hardcoding it, because vpcd readers pay ~2 s/APDU
  (docs/VPCD_WIFI_RUNBOOK.md) and cannot afford USB-sized warmups.
* The CPython cyclic GC is disabled inside timed regions (a GC pass
  is a multi-hundred-microsecond outlier source in ms-scale samples);
  collect-then-disable, restore afterwards — same practice as pyperf
  (https://github.com/psf/pyperf) and
  https://superfastpython.com/python-benchmarking-best-practices/.
* p50/p90/p99/max are recorded, never a bare mean: the tail is the
  regression signal (Gil Tene, "How NOT to Measure Latency",
  https://www.infoq.com/presentations/latency-user-perspective;
  coordinated omission background:
  https://www.scylladb.com/2021/04/22/on-coordinated-omission).
  The workload is a closed sequential stream — each sample is a full
  issue->completion interval, so there is no coordinated omission by
  construction.
* Card presence is probed with a REAL SCardConnect (ATR or PC/SC
  error); the pcscd GetStatusChange cache lies after daemon restarts —
  docs/VPCD_WIFI_RUNBOOK.md "Operational rules" #2.
* APDU shapes are read-only (SELECT / GET DATA) and borrowed from the
  in-repo predecessors tests/hardware/perf/benchmark_readers.py and
  tests/hardware/nfc/perf_ab.py. No dedicated open-source
  smartcard-reader benchmark standard exists to copy (checked
  2026-10-13); the vpcd upstream vsmartcard
  (https://github.com/frankmorgner/vsmartcard) ships no bench harness
  either, so the statistical practice above is what this suite
  borrows from.
"""

import contextlib
import gc
import json
import os
import time
from datetime import datetime
from pathlib import Path

#: The suite lives here; the ledger sits next to it (gitignored).
SUITE_DIR = Path(__file__).resolve().parent
LEDGER_DEFAULT = SUITE_DIR / "results" / "perf-history.jsonl"

#: Seconds to let pcscd settle between ATR-latency samples (outside all
#: timed regions). Keeps reconnect churn from bleeding into samples.
RECONNECT_SETTLE_S = 0.3


# ---------------------------------------------------------------------------
# PARAMS — the single tuning table (README.md "Parameters")
# ---------------------------------------------------------------------------
#: Iteration counts scale per reader class: the ACR1252 reference does a
#: 50-APDU stream in under a second, while vpcd transports pay ~2 s/APDU
#: (docs/VPCD_WIFI_RUNBOOK.md "Known limitations") and must stay in the
#: single digits per metric to keep a full pass minutes-scale.
PARAMS = {
    # env knobs (see README.md); the NUCULA_PERF gate keeps the suite
    # skipped everywhere except the bench opt-in.
    "env": {
        "gate": "NUCULA_PERF",                    # =1 enables real runs
        "readers": "NUCULA_PERF_READERS",         # comma list, like repeated --reader
        "iterations": "NUCULA_PERF_ITERATIONS",   # overrides every class's stream/RTT count
        "atr_samples": "NUCULA_PERF_ATR_SAMPLES", # overrides every class's ATR sample count
        "warmup": "NUCULA_PERF_WARMUP",           # overrides every class's warmup count
        "fw_rev": "NUCULA_PERF_FW_REV",          # recorded verbatim in ledger rows
        "ledger": "NUCULA_PERF_LEDGER",           # alternate ledger path
    },
    "iterations_default": 50,   # NUCULA_PERF_ITERATIONS default (USB-class)
    "atr_samples_default": 10,
    "warmup_default": 3,
    # Known reader classes. "match" substrings must ALL appear in the
    # pcscd reader name (case-insensitive). Bench names (2026-10-13):
    # "ACS ACR1252 Dual Reader [ACS ACR1252 Dual Reader PICC] 01 00" —
    # the PICC (contactless) interface, not the SAM one;
    # "Nucula VPCD 00 00" / "Nucula VPCD 00 01" (runbook "Multi-board
    # benches").
    "reader_classes": {
        "acr1252": {
            "match": ("ACR1252", "PICC"),
            "transport": "usb-ccid",
            "iterations": 50,   # ~9 ms/APDU on the bench: seconds per metric
            "atr_samples": 10,
            "warmup": 3,
        },
        "nucula-vpcd": {
            "match": ("Nucula VPCD",),
            "transport": "wifi-vpcd",
            "iterations": 10,   # ~2 s/APDU: 10 iterations ~= 20 s per metric
            "atr_samples": 3,
            "warmup": 1,
        },
    },
    # Fallback class for any other card-bearing reader pcscd lists
    # ("default: all discovered incl. ACR1252" — e.g. Cherry, CardMan,
    # NR7101 when present on the bench). USB-class defaults; transport
    # guessed from the name.
    "generic_class": {
        "match": (),
        "transport": None,
        "iterations": 50,
        "atr_samples": 10,
        "warmup": 3,
    },
}


# ---------------------------------------------------------------------------
# APDUs — read-only by construction (README.md "APDU safety")
# ---------------------------------------------------------------------------
#: OpenPGP applet AID (task doctrine; also the SELECT probe of
#: tests/hardware/nfc/cardlib.py AID_PROBES["OpenPGP"], extended form).
OPENPGP_AID = bytes.fromhex("D276000124010304AFAF000000000000")

#: RTT probes: SELECT the OpenPGP applet, then GET DATA tag 004F
#: (application identifier) inside the selected applet. Both are
#: read-only on the OpenPGP applet (ISO 7816-4 SELECT/GET DATA).
RTT_APDUS = (
    ("select", bytes([0x00, 0xA4, 0x04, 0x00, len(OPENPGP_AID)]) + OPENPGP_AID + bytes([0x00])),
    ("get_data", bytes.fromhex("00CA004F00")),
)

#: Throughput stream payloads (SELECT by DF name — a lookup; answers
#: 6A82 "not found" on cards without the applet, touching no state):
#: small = exactly 6 B (case 3, Lc=1), large = exactly 255 B (case 3,
#: Lc=250; the runbook documents that vpcd answers SW 64 00 for APDUs
#: above the ~245 B NCI single-packet cap — the stream still measures
#: transport throughput; see README.md).
STREAM_SMALL_APDU = bytes([0x00, 0xA4, 0x04, 0x00, 0x01, 0x00])            # 6 B
_LARGE_DATA = OPENPGP_AID + b"\x00" * (250 - len(OPENPGP_AID))             # 250 B
STREAM_LARGE_APDU = bytes([0x00, 0xA4, 0x04, 0x00, 250]) + _LARGE_DATA     # 255 B

#: Anything that mutates card state (VERIFY, UPDATE, GP DELETE/INSTALL,
#: STORE DATA, ...) is forbidden here — enforced by review, not runtime.


def assert_safe_apdus():
    """Tripwire for accidental destructive payloads: every APDU this
    suite sends must be a SELECT (INS A4) or GET DATA (INS CA) — the
    two read-only commands used anywhere in this suite."""
    for _, apdu in RTT_APDUS:
        assert apdu[1] in (0xA4, 0xCA), f"unsafe INS in RTT set: {apdu.hex()}"
    for apdu in (STREAM_SMALL_APDU, STREAM_LARGE_APDU):
        assert apdu[1] == 0xA4, f"non-SELECT stream APDU: {apdu.hex()}"
    assert len(STREAM_SMALL_APDU) == 6
    assert len(STREAM_LARGE_APDU) == 255


# ---------------------------------------------------------------------------
# statistics
# ---------------------------------------------------------------------------

def percentiles(values, ps=(50, 90, 99)):
    """Nearest-rank percentiles of a plain list — same estimator as the
    in-repo predecessor tests/hardware/perf/benchmark_readers.py, so
    ledger numbers stay comparable with the 2026-10-10 baseline table
    in docs/PERF_AND_DRY_STRATEGY.md."""
    xs = sorted(values)
    out = {}
    for p in ps:
        k = min(len(xs) - 1, int(round(p / 100 * (len(xs) - 1))))
        out[p] = xs[k]
    return out


def summarize(values):
    """Ledger stats block: n, p50, p90, p99, max (ms-scale, 3 decimals)."""
    if not values:
        return {"n": 0, "p50": None, "p90": None, "p99": None, "max": None}
    p = percentiles(values, (50, 90, 99))
    return {
        "n": len(values),
        "p50": round(p[50], 3),
        "p90": round(p[90], 3),
        "p99": round(p[99], 3),
        "max": round(max(values), 3),
    }


@contextlib.contextmanager
def no_gc():
    """collect() then disable GC for the timed region; restore after.

    A CPython GC pass mid-sample is a classic latency outlier in
    ms-scale benchmarking — pyperf disables GC the same way
    (https://pyperf.readthedocs.io/en/latest/cli.html,
    https://superfastpython.com/python-benchmarking-best-practices/).
    """
    was_enabled = gc.isenabled()
    gc.collect()
    gc.disable()
    try:
        yield
    finally:
        if was_enabled:
            gc.enable()


def effective_params(cls_params):
    """Per-class PARAMS merged with the global env overrides
    (NUCULA_PERF_ITERATIONS / _ATR_SAMPLES / _WARMUP win over the
    per-class defaults)."""
    env = PARAMS["env"]
    out = dict(cls_params)
    for key, env_name, default_key in (
        ("iterations", "iterations", "iterations_default"),
        ("atr_samples", "atr_samples", "atr_samples_default"),
        ("warmup", "warmup", "warmup_default"),
    ):
        out[key] = out.get(key, PARAMS[default_key])
        raw = os.environ.get(env[env_name], "").strip()
        if raw:
            out[key] = max(1, int(raw))
    return out


# ---------------------------------------------------------------------------
# reader discovery (real-connect presence gate — runbook rule #2)
# ---------------------------------------------------------------------------

def transport_guess(name):
    """Transport tag recorded in the ledger, guessed from the pcscd
    reader name."""
    n = name.lower()
    if "vpcd" in n:
        return "wifi-vpcd"
    if "gempctwin" in n or "serial" in n:
        return "serial-ccid"
    return "usb-ccid"


def classify(name):
    """Reader class for a pcscd name: first PARAMS class whose match
    substrings all appear; else "generic"."""
    for cls, cfg in PARAMS["reader_classes"].items():
        if all(m.lower() in name.lower() for m in cfg["match"]):
            return cls
    return "generic"


def _pcsc_readers():
    from smartcard.System import readers  # lazy: collection-safe import
    return list(readers())


def real_connect_probe(reader_obj, attempts=2, delay=1.0):
    """Presence = REAL SCardConnect (ATR or PC/SC error), per runbook
    "Operational rules" #2 — NEVER GetStatusChange, whose cached state
    lies after pcscd restarts. Some readers report 'unpowered' on the
    first attempt and power up on the second
    (tests/hardware/nfc/cardlib.py connect_reader precedent).

    Returns (has_card: bool, atr: bytes).
    """
    from smartcard.Exceptions import CardConnectionException, NoCardException

    last_err = None
    for _ in range(attempts):
        conn = reader_obj.createConnection()
        try:
            conn.connect()
            return True, bytes(conn.getATR())
        except NoCardException:
            return False, b""
        except CardConnectionException as e:
            last_err = e
            time.sleep(delay)
        finally:
            try:
                conn.disconnect()
            except Exception:
                pass
    print(f"  [perf] {reader_obj}: dead slot ({str(last_err)[:60]})")
    return False, b""


def discover_readers():
    """Every pcscd reader -> target dict with real-connect presence,
    class, effective params, transport. No filtering here; selection
    happens in filter_readers()."""
    targets = []
    for r in _pcsc_readers():
        name = str(r)
        cls = classify(name)
        cls_params = (PARAMS["reader_classes"].get(cls)
                      or PARAMS["generic_class"])
        has_card, atr = real_connect_probe(r)
        targets.append({
            "reader_obj": r,
            "name": name,
            "cls": cls,
            "has_card": has_card,
            "atr_hex": atr.hex().upper(),
            "params": effective_params(cls_params),
            "transport": cls_params.get("transport") or transport_guess(name),
        })
    return targets


def filter_readers(filters, targets):
    """Apply --reader / NUCULA_PERF_READERS values.

    A target is kept when ANY filter value matches (case-insensitive):
    the pcscd reader name, the class id, or one of the class's PARAMS
    match substrings. So --reader=acr1252 selects the class,
    --reader="Nucula VPCD 00 01" narrows to the one slot,
    --reader=PICC hits only the ACR1252 contactless interface.
    No filters -> everything discovered (incl. generic readers).
    """
    if not filters:
        return list(targets)
    fl = [f.lower() for f in filters if f and f.strip()]
    kept = []
    for t in targets:
        match_keys = PARAMS["reader_classes"].get(t["cls"], {}).get("match", ())
        if (any(f in t["name"].lower() for f in fl)
                or any(f in t["cls"].lower() for f in fl)
                or any(f in m.lower() for f in fl for m in match_keys)):
            kept.append(t)
    return kept


def connect_reader_obj(reader_obj, attempts=3, delay=1.5):
    """createConnection + SCardConnect + getATR on a live reader object.

    The whole call IS the time-to-ATR sample (connect() triggers
    IccPowerOn; getATR() reads the answer) — test_atr_latency.py times
    exactly this. Retry shape from cardlib.connect_reader (readers that
    report 'unpowered' usually power up on the second attempt).
    Returns (conn, atr_bytes).
    """
    from smartcard.Exceptions import CardConnectionException, NoCardException

    last = None
    for _ in range(attempts):
        conn = reader_obj.createConnection()
        try:
            conn.connect()
            return conn, bytes(conn.getATR())
        except (NoCardException, CardConnectionException) as e:
            last = e
            time.sleep(delay)
            try:
                conn.disconnect()
            except Exception:
                pass
    raise last


def open_connection(name, attempts=3, delay=1.5):
    """Same as connect_reader_obj but resolves the pcscd reader by exact
    name first (one SCardListReaders round-trip, OUTSIDE any timed
    region)."""
    from smartcard.Exceptions import CardConnectionException

    rs = [r for r in _pcsc_readers() if str(r) == name]
    if not rs:
        raise CardConnectionException(f"reader {name!r} not listed")
    return connect_reader_obj(rs[0], attempts, delay)


def transmit(conn, apdu):
    """Send one command APDU; returns (resp_bytes, sw_int).

    sw_int = sw1<<8 | sw2. The suite treats any returned SW (including
    vpcd's documented 64 00 transport failure) as a completed exchange
    — the metric is transport latency/throughput, not card semantics.
    Exceptions propagate (caller's error policy: 3 strikes -> stop).
    """
    data, sw1, sw2 = conn.transmit(list(apdu))
    return bytes(data or b""), (sw1 << 8) | sw2


# ---------------------------------------------------------------------------
# ledger
# ---------------------------------------------------------------------------

class PerfLedger:
    """Append-only JSONL results ledger.

    Row schema (exactly these keys; README.md documents the null
    semantics for aggregate metrics):
      {ts, reader, transport, fw_rev, metric, n, p50, p90, p99, max}
    """

    KEYS = ("ts", "reader", "transport", "fw_rev", "metric", "n",
            "p50", "p90", "p99", "max")

    def __init__(self, path, fw_rev=None):
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        # fw_rev is not queryable through pcscd without reader-specific
        # escape APDUs that would pollute the stream; an operator can
        # pin it with NUCULA_PERF_FW_REV.
        self.fw_rev = fw_rev

    def _append(self, row):
        with self.path.open("a", encoding="utf-8") as f:
            f.write(json.dumps({k: row[k] for k in self.KEYS}) + "\n")

    @staticmethod
    def _stamp():
        return datetime.now().astimezone().isoformat(timespec="seconds")

    def record_samples(self, reader, transport, metric, values):
        """Stats row from raw per-sample ms values (warmup discarded)."""
        s = summarize(values)
        row = {"ts": self._stamp(), "reader": reader,
               "transport": transport, "fw_rev": self.fw_rev,
               "metric": metric, **s}
        self._append(row)
        print(f"  [ledger] {reader} :: {metric} n={s['n']} "
              f"p50={s['p50']} p90={s['p90']} p99={s['p99']} max={s['max']}")
        return row

    def record_aggregate(self, reader, transport, metric, value, n):
        """Single-value metric (e.g. APDUs/sec): p50 carries the value,
        p90/p99/max are null (an aggregate has no distribution)."""
        row = {"ts": self._stamp(), "reader": reader,
               "transport": transport, "fw_rev": self.fw_rev,
               "metric": metric, "n": n, "p50": round(value, 3),
               "p90": None, "p99": None, "max": None}
        self._append(row)
        print(f"  [ledger] {reader} :: {metric} n={n} value={row['p50']}")
        return row


# ---------------------------------------------------------------------------
# dry-run plan (no pcscd / no pyscard access)
# ---------------------------------------------------------------------------

def build_plan(cli_readers=None):
    """Human-readable plan of what a real run would do (--dry-run)."""
    env = PARAMS["env"]
    filters = cli_readers or [s.strip() for s in
                              os.environ.get(env["readers"], "").split(",")
                              if s.strip()]
    lines = [
        "=== smartcard perf suite — DRY RUN (no pcscd/pyscard access) ===",
        f"gate: {env['gate']}={'1 (real run)' if os.environ.get(env['gate']) == '1' else 'unset (tests would skip)'}",
        f"selection: {' '.join(repr(f) for f in filters) if filters else 'default — all discovered card-bearing readers'}",
        "reader classes (iteration counts already include env overrides):",
    ]
    for cls, cfg in list(PARAMS["reader_classes"].items()) + [("generic", PARAMS["generic_class"])]:
        eff = effective_params(cfg)
        match = ", ".join(cfg["match"]) or "(any other card-bearing reader)"
        lines.append(
            f"  {cls:12s} match[{match}] transport={eff.get('transport') or 'guess-from-name'}"
            f" iterations={eff['iterations']} warmup={eff['warmup']} atr_samples={eff['atr_samples']}")
    lines += [
        "APDUs (read-only): RTT " + " / ".join(f"{n}={a.hex().upper()}" for n, a in RTT_APDUS),
        f"                streams small({len(STREAM_SMALL_APDU)}B)={STREAM_SMALL_APDU.hex().upper()}"
        f" large({len(STREAM_LARGE_APDU)}B)={STREAM_LARGE_APDU[:8].hex().upper()}...",
        f"metrics: atr_latency_ms, apdu_rtt_select_ms, apdu_rtt_get_data_ms,",
        f"         stream_{{small,large}}_apdu_ms, throughput_{{small,large}}_apdus_per_sec,",
        f"         throughput_{{small,large}}_bytes_per_sec",
        f"ledger: {LEDGER_DEFAULT}",
        "real run: NUCULA_PERF=1 python3 -m pytest tests/hardware/perf/ -v",
        "report:   python3 tests/hardware/perf/report.py",
        "=== end of plan ===",
    ]
    return "\n".join(lines)
