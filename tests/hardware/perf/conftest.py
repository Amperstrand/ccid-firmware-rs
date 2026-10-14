"""Pytest plumbing for the parameterized reader perf suite.

Everything here encodes docs/VPCD_WIFI_RUNBOOK.md + README.md (this
directory):

* Whole suite skipped unless NUCULA_PERF=1 (bench opt-in; the bench is
  shared and this suite talks to real cards).
* --dry-run / --list prints the plan (classes, iteration counts, APDUs,
  ledger path) and skips every test WITHOUT touching pcscd or pyscard.
* Reader selection: repeated --reader=<substr> or NUCULA_PERF_READERS
  (comma list); default = every discovered card-bearing reader,
  including the ACR1252 reference (its rows anchor every comparison
  table report.py prints).
* Card presence per reader = REAL SCardConnect (runbook "Operational
  rules" #2) — done once at session discovery in perflib, never via
  the lying GetStatusChange cache.

Plain pytest + pyscard only. Options are perf-suite-local names
(--reader, --dry-run, --list) — no clashes with the --hil / --readers
options of the sibling suites.
"""

import os

import pytest

from .perflib import (LEDGER_DEFAULT, PARAMS, PerfLedger, assert_safe_apdus,
                      build_plan, discover_readers, filter_readers)


def pytest_addoption(parser):
    parser.addoption(
        "--reader", action="append", default=None, metavar="SUBSTR",
        help="Reader filter, repeatable: case-insensitive substring of the "
             "pcscd reader name, the class id (acr1252, nucula-vpcd, "
             "generic) or a class match key (e.g. PICC). Default: all "
             "discovered card-bearing readers. Env alternative: "
             "NUCULA_PERF_READERS (comma list).")
    parser.addoption(
        "--dry-run", "--list", dest="perf_dry_run", action="store_true",
        default=False,
        help="Print what a real run would measure (classes, iteration "
             "counts, APDUs, ledger path) and skip all tests. No "
             "pcscd/pyscard access.")


def pytest_configure(config):
    config.addinivalue_line(
        "markers", "perf: smartcard reader performance measurement")
    # Safety tripwire: collection fails if a destructive (non SELECT/GET DATA) APDU is ever added.
    assert_safe_apdus()
    if config.getoption("perf_dry_run"):
        # Plan print, not a measurement: safe on a busy bench and in CI.
        print(build_plan(cli_readers=config.getoption("--reader")))


def _selection_filters(request):
    """CLI --reader values win over NUCULA_PERF_READERS; None = default."""
    cli = request.config.getoption("--reader")
    if cli:
        return cli
    env_raw = os.environ.get(PARAMS["env"]["readers"], "")
    return [s.strip() for s in env_raw.split(",") if s.strip()] or None


@pytest.fixture(autouse=True, scope="session")
def perf_gate(request):
    """Skip-all gate. Runs before every other session fixture (they all
    depend on this one), so a skipped suite never touches pcscd."""
    if request.config.getoption("perf_dry_run"):
        pytest.skip("perf --dry-run: plan printed at session start, "
                    "no measurements taken")
    if os.environ.get(PARAMS["env"]["gate"]) != "1":
        pytest.skip(f"{PARAMS['env']['gate']} != 1 — reader perf suite "
                    "disabled (preview: pytest tests/hardware/perf/ "
                    "--dry-run)")


@pytest.fixture(scope="session")
def perf_selection(request, perf_gate):
    return _selection_filters(request)


@pytest.fixture(scope="session")
def perf_readers(perf_gate, perf_selection):
    """Live targets: every discovered card-bearing reader that survives
    the selection filters. Discovery does the real-connect presence
    gate (perflib.real_connect_probe)."""
    targets = discover_readers()
    selected = filter_readers(perf_selection, targets)
    live = [t for t in selected if t["has_card"]]
    cardless = [t["name"] for t in selected if not t["has_card"]]
    if cardless:
        print(f"  [perf] cardless (skipped, real-connect probe): {cardless}")
    if not live:
        pytest.skip(
            f"no card-bearing reader matched "
            f"{perf_selection or 'default (all discovered)'} — "
            f"pcscd listed: {[t['name'] for t in targets]}")
    for t in live:
        p = t["params"]
        print(f"  [perf] target {t['name']} (class={t['cls']}, "
              f"transport={t['transport']}, ATR={t['atr_hex'] or '?'}, "
              f"iterations={p['iterations']}, warmup={p['warmup']}, "
              f"atr_samples={p['atr_samples']})")
    return live


@pytest.fixture(scope="session")
def ledger(perf_gate):
    """Ledger recorder; NUCULA_PERF_LEDGER overrides the default path.
    NUCULA_PERF_FW_REV, when set, is stamped on every row (not
    queryable through pcscd without reader-specific escapes)."""
    path = os.environ.get(PARAMS["env"]["ledger"]) or LEDGER_DEFAULT
    led = PerfLedger(path, fw_rev=os.environ.get(PARAMS["env"]["fw_rev"]) or None)
    yield led
    print(f"\n[perf] ledger written: {path}")
    print("[perf] comparison table: python3 tests/hardware/perf/report.py"
          + (f" --ledger {path}" if path != LEDGER_DEFAULT else ""))


@pytest.fixture(params=list(PARAMS["reader_classes"]) + ["generic"],
                ids=list(PARAMS["reader_classes"]) + ["generic"])
def reader_target(request, perf_readers):
    """Live card-bearing readers of one PARAMS class (parametrized so
    --collect-only shows the full matrix without hardware). The class
    list is static; which readers it expands to is decided at runtime
    by real-connect discovery."""
    cls = request.param
    targets = [t for t in perf_readers if t["cls"] == cls]
    if not targets:
        pytest.skip(f"no card-bearing reader of class {cls!r} in this run")
    return targets
