"""Offline tests for track_d (plan todo 13) — no pyscard, no hardware,
no pcscd. pyscard is stubbed in sys.modules before any suite import so the
real library is never touched even where installed."""

import json
import sys
import threading
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import track_d  # noqa: E402


@pytest.fixture
def fresh_suites(monkeypatch):
    """Evict cached suite/soaklib modules and stub pyscard before import."""
    for name in list(sys.modules):
        if name == "soaklib" or name.startswith("esp32_overnight_"):
            del sys.modules[name]
    track_d.stub_smartcard(lambda d, k, v: monkeypatch.setitem(d, k, v))
    yield


# ------------------------------------------------------------ key audit ----

def test_audit_real_suites_all_included():
    audit = track_d.audit_soak_tests()
    by = {}
    for e in audit["entries"]:
        by.setdefault(e["suite"], {})[e["test"]] = e
    expected = {"soak_01_smoke": 6, "soak_09_stress": 7,
                "soak_10_cross_compare": 14}
    for suite, count in expected.items():
        tests = by[suite]
        assert len(tests) == count, (suite, len(tests))
        bad = {t: e["verdict"] for t, e in tests.items()
               if e["verdict"] != "INCLUDED"}
        assert not bad, (suite, bad)
    for t in track_d.SUITE_08_BOUNDARY_SUBSET:
        assert by[track_d.SUITE_08][t]["verdict"] == "INCLUDED"
    included = [e for e in audit["entries"] if e["verdict"] == "INCLUDED"]
    assert len(included) == 29
    # INCLUDED verdicts carry per-test APDU evidence (the audit is real)
    with_apdus = [e for e in included if e["suite"] != "soak_01_smoke"
                  or e["test"] != "connect_and_atr"]
    assert any("APDU" in ev or "INS" in ev
               for e in with_apdus for ev in e["evidence"])


def test_audit_excludes_foreign_key_suites_with_evidence():
    audit = track_d.audit_soak_tests()
    by = {}
    for e in audit["entries"]:
        by.setdefault(e["suite"], []).append(e)
    for suite in ("soak_02_globalplatform", "soak_03_satochip",
                  "soak_04_openpgp", "soak_05_piv", "soak_06_opensc",
                  "soak_07_gpg"):
        tests = by[suite]
        assert tests, suite
        assert all(e["verdict"] == "EXCLUDED" for e in tests), suite
        assert all(e["key_risk"] not in ("none",) for e in tests), suite
        assert all(e["evidence"] for e in tests), suite
    mod = {m["suite"]: m for m in audit["modules"]}
    assert mod["soak_03_satochip"]["module_key_risk"] == "module-marker"
    assert any("PIN_TEST" in m for m in mod["soak_03_satochip"]["markers"])
    assert any("VERIFY_PIN" in m for m in mod["soak_05_piv"]["markers"])


def test_audit_scope_exclusions_and_fail_closed_unresolved():
    audit = track_d.audit_soak_tests()
    by = {}
    for e in audit["entries"]:
        by.setdefault(e["suite"], {})[e["test"]] = e
    out_of_subset = [e for e in by[track_d.SUITE_08].values()
                     if e["test"] not in track_d.SUITE_08_BOUNDARY_SUBSET]
    assert all(e["verdict"] == "EXCLUDED" for e in out_of_subset)
    key_free = [e for e in out_of_subset if e["key_risk"] == "none"]
    assert key_free
    assert all("boundary subset" in e["reason"] for e in key_free)
    # bytes.fromhex("00CA" + tag + "00") is not provable -> fail closed
    assert by[track_d.SUITE_08]["get_data_empty"]["key_risk"] == "unresolved"
    # suite 11 exists but is not in tonight's selection
    assert all(e["verdict"] == "EXCLUDED"
               for e in by["soak_11_cherry"].values())


def _write_suite(path, body):
    path.write_text(body)
    return path


def test_audit_detects_injected_verify_apdu(tmp_path):
    p = _write_suite(tmp_path / "soak_99a.py", (
        "from soaklib import transmit_apdu\n"
        "SELECT_X = bytes.fromhex('0020008008') + b'12345678' + b'00'\n"
        "def test_pin(label, reader_info, conn):\n"
        "    data, sw = transmit_apdu(conn, SELECT_X)\n"
        "    return {'sw': sw}\n"
        "def main():\n"
        "    run_test_on_both(suite, 'pin', test_pin, readers)\n"))
    _, entries = track_d.audit_suite_file(p)
    e = entries[0]
    assert e.verdict == "EXCLUDED"
    assert e.key_risk == "auth-apdu/library"
    assert any("INS=20 VERIFY" in ev for ev in e.evidence)


def test_audit_detects_ntag_authenticate_apdu(tmp_path):
    p = _write_suite(tmp_path / "soak_99b.py", (
        "from soaklib import transmit_apdu\n"
        "AUTH_FIRST = bytes.fromhex('90AF0000020000')\n"
        "def test_auth(label, reader_info, conn):\n"
        "    data, sw = transmit_apdu(conn, AUTH_FIRST)\n"
        "    return {'sw': sw}\n"
        "def main():\n"
        "    run_test_on_both(suite, 'auth', test_auth, readers)\n"))
    _, entries = track_d.audit_suite_file(p)
    assert entries[0].verdict == "EXCLUDED"
    assert any("NTAG424 AUTHENTICATE" in ev for ev in entries[0].evidence)


def test_audit_detects_pin_library_call(tmp_path):
    p = _write_suite(tmp_path / "soak_99c.py", (
        "def test_pin(label, reader_info, conn):\n"
        "    pysc.verify_pin(b'1234')\n"
        "    return {'ok': 1}\n"
        "def main():\n"
        "    run_test_on_both(suite, 'pin', test_pin, readers)\n"))
    _, entries = track_d.audit_suite_file(p)
    assert entries[0].verdict == "EXCLUDED"
    assert any("auth-library call" in ev for ev in entries[0].evidence)


# ------------------------------------------------------------------ gate ----

def _mini_entries():
    reviewed = [
        {"suite": "s", "test": "ok", "verdict": "INCLUDED", "reason": "",
         "evidence": []},
        {"suite": "s", "test": "bad", "verdict": "EXCLUDED",
         "reason": "auth", "evidence": []},
    ]
    fresh_same = [
        {"suite": "s", "test": "ok", "verdict": "INCLUDED"},
        {"suite": "s", "test": "bad", "verdict": "EXCLUDED"},
    ]
    return reviewed, fresh_same


def test_gate_refuses_excluded_missing_drifted():
    reviewed, fresh = _mini_entries()
    gate = track_d.SoakGate(reviewed, fresh)
    gate.check("s", "ok")
    with pytest.raises(track_d.SoakGateError, match="EXCLUDED"):
        gate.check("s", "bad")
    with pytest.raises(track_d.SoakGateError, match="no reviewed"):
        gate.check("s", "never_audited")
    drift = track_d.SoakGate(reviewed, [
        {"suite": "s", "test": "ok", "verdict": "EXCLUDED",
         "reason": "drifted"}])
    with pytest.raises(track_d.SoakGateError, match="source drift"):
        drift.check("s", "ok")
    assert gate.included_tests("s") == ["ok"]
    assert drift.included_tests("s") == []


def test_gate_from_files_matches_committed_allowlist():
    gate = track_d.SoakGate.from_files()
    plan = track_d.build_plan(gate)
    assert len(plan) == 29
    suites_in_order = [s for s in dict.fromkeys(s for s, _ in plan)]
    assert suites_in_order == ["soak_01_smoke", "soak_09_stress",
                               "soak_10_cross_compare",
                               "soak_08_extended_apdu"]
    for suite, test in plan:
        gate.check(suite, test)


def test_gate_refuses_when_suite_source_drifts(tmp_path):
    soak = tmp_path / "soak"
    soak.mkdir()
    for src in (track_d.SOAK_DIR).glob("*.py"):
        (soak / src.name).write_text(src.read_text())
    # reviewed allowlist says select_mf is INCLUDED...
    reviewed = [e for e in json.loads(
        track_d.ALLOWLIST_PATH.read_text())["entries"]
        if e["suite"] == "soak_01_smoke"]
    # ...but the suite source now sends a VERIFY (INS 0x20) as its SELECT
    sm = soak / "soak_01_smoke.py"
    sm.write_text(sm.read_text().replace("00A4040000", "0020000000"))
    fresh = track_d.audit_soak_tests(soak)
    gate = track_d.SoakGate(reviewed, fresh["entries"])
    with pytest.raises(track_d.SoakGateError, match="source drift"):
        gate.check("soak_01_smoke", "select_mf")
    # auth constant named PIN-style also poisons the whole module (fail
    # closed at module level)
    sm.write_text(sm.read_text() + (
        "\nPIN_STASH = '31323334'\n"
        "def test_extra(label, reader_info, conn):\n"
        "    return {}\n"
        "run_test_on_both(suite, 'extra', test_extra, readers)\n"))
    fresh2 = track_d.audit_soak_tests(soak)
    gate2 = track_d.SoakGate(reviewed, fresh2["entries"])
    assert gate2.included_tests("soak_01_smoke") == []


# ------------------------------------------------------------ diagnostics ---

def test_parse_diagnostics_exact_layout():
    # mirror of crates/ccid-core/src/diagnostics.rs field_offsets test
    words = (0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 1, 0x0F)
    snap = track_d.parse_diagnostics(
        b"".join(w.to_bytes(4, "little") for w in words))
    assert snap.apdu_tx_count == 0x0A
    assert snap.apdu_rx_count == 0x0B
    assert snap.nak_count == 0x0C
    assert snap.error_count == 0x0D
    assert snap.reinit_count == 0x0E
    assert snap.card_present is True
    assert snap.uptime_ticks == 0x0F


def test_parse_diagnostics_short_rejected_longer_accepted():
    with pytest.raises(ValueError):
        track_d.parse_diagnostics(b"\x00" * 27)
    snap = track_d.parse_diagnostics(b"\x00" * 64)
    assert snap.uptime_ticks == 0 and snap.card_present is False


def test_check_monotonic_violation_detection():
    def s(tx, rx, nak, err, reinit, up):
        return track_d.DiagnosticsSnapshot(tx, rx, nak, err, reinit, True, up)

    assert track_d.check_monotonic(
        s(1, 1, 0, 0, 0, 10), s(5, 5, 2, 1, 0, 99)) == []
    v = track_d.check_monotonic(s(9, 2, 1, 0, 0, 50), s(4, 3, 0, 1, 2, 49))
    assert any("apdu_tx_count regressed" in x for x in v)
    assert any("nak_count regressed" in x for x in v)
    assert any("reinit_count changed" in x for x in v)
    assert any("uptime_ticks regressed" in x for x in v)
    # equal counters are legal (only reinit DELTA and uptime regression fail)
    assert track_d.check_monotonic(
        s(5, 5, 5, 5, 0, 7), s(5, 5, 5, 5, 0, 7)) == []


# ----------------------------------------------------------- reader gate ----

def test_reader_gate_requires_both_readers():
    ok = track_d.check_readers(
        ["GemPCTwin serial 00 00", "ACS ACR1252 00 00", "ACS ACR1252 01 00"])
    assert ok.ok and ok.gempctwin == "GemPCTwin serial 00 00"
    assert not track_d.check_readers(["ACS ACR1252 00 00"]).ok
    assert not track_d.check_readers(["GemPCTwin serial 00 00"]).ok
    assert not track_d.check_readers([]).ok
    assert "GemPCTwin" in track_d.check_readers(
        ["ACS ACR1252 00 00"]).detail


def test_pick_reader_prefers_picc_channel_never_sam():
    names = ["ACS ACR1252 01 00", "ACS ACR1252 00 00"]
    assert track_d.pick_reader(names, "ACR1252") == "ACS ACR1252 00 00"
    assert track_d.pick_reader(names, "ACR1252", avoid="01 00") == \
        "ACS ACR1252 00 00"
    # SAM-only: avoid never eliminates the last candidate
    only_sam = ["ACS ACR1252 01 00"]
    assert track_d.pick_reader(only_sam, "ACR1252") == "ACS ACR1252 01 00"
    assert track_d.pick_reader(only_sam, "ACR1252", avoid="01 00") == \
        "ACS ACR1252 01 00"
    assert track_d.pick_reader(names, "nonexistent") is None


# -------------------------------------------------------------- ATR loop ----

def test_atr_stability_byte_equal_passes():
    summary = track_d.atr_stability_loop(
        track_d.FakeConn(), iterations=25, pace_s=0.0)
    assert summary["stable"] is True
    assert summary["iterations_done"] == 25
    assert summary["finding"] is None
    assert summary["stopped_reason"] == "completed"
    assert summary["atr_hex"] == "3b8580018073c821100e"


def test_atr_stability_first_mismatch_is_finding():
    flaky = track_d.FakeConn()
    calls = {"n": 0}
    real = flaky.reconnect

    def flip():
        real()
        calls["n"] += 1
        if calls["n"] == 3:
            flaky.atr = bytes.fromhex("3b00")
    flaky.reconnect = flip
    rows = []
    summary = track_d.atr_stability_loop(
        flaky, iterations=10, pace_s=0.0, emit=lambda **f: rows.append(f))
    assert summary["stable"] is False
    assert summary["stopped_reason"] == "atr_mismatch"
    assert summary["finding"]["iteration"] == 2
    assert summary["finding"]["expected"] == "3b8580018073c821100e"
    assert summary["finding"]["got"] == "3b00"


def test_atr_stability_reconnect_errors_capped():
    class DeadConn(track_d.FakeConn):
        def reconnect(self):
            raise RuntimeError("reader gone")
    rows = []
    summary = track_d.atr_stability_loop(
        DeadConn(), iterations=500, pace_s=0.0, emit=lambda **f: rows.append(f))
    assert summary["stopped_reason"] == "too_many_reconnect_errors"
    assert summary["reconnect_errors"] == track_d.MAX_RECONNECT_ERRORS
    assert rows and rows[0]["type"] == "atr_reconnect_error"


# ------------------------------------------------------- budget/pause -------

def test_iteration_budget_and_max_iters():
    clock = {"t": 0.0}
    rows = []

    def run_one(suite, test, i):
        clock["t"] += 1.0
        return {"type": "soak_test", "status": "PASS", "suite": suite,
                "test": test}

    def sleep(s):
        clock["t"] += s

    stats = track_d.run_soak_iterations(
        run_one, [("a", "x")], max_iters=99, budget_s=3.5,
        now_fn=lambda: clock["t"], sleep_fn=sleep, pace_s=0.0,
        emit=rows.append)
    assert stats["iterations"] == 4
    assert stats["stopped_reason"] == "time_budget_exhausted"
    assert stats["pass"] == 4
    skip_rows = [r for r in rows if r.get("reason") == "time budget exhausted"]
    assert skip_rows and skip_rows[0]["iteration"] == 4

    rows.clear()
    stats2 = track_d.run_soak_iterations(
        run_one, [("a", "x"), ("a", "y")], max_iters=2, budget_s=1e9,
        now_fn=lambda: clock["t"], sleep_fn=sleep, pace_s=0.0,
        emit=rows.append)
    assert stats2["iterations"] == 2 and stats2["pass"] == 4
    assert len([r for r in rows if r["type"] == "soak_iter"]) == 2


def test_pause_gate_blocks_operations():
    track_d.pause()
    assert track_d.PAUSE_GATE.is_paused()
    t0 = time.monotonic()

    def resumer():
        time.sleep(0.1)
        track_d.resume()
    th = threading.Thread(target=resumer)
    th.start()
    track_d.PAUSE_GATE.wait_while_paused()
    th.join()
    assert time.monotonic() - t0 >= 0.05
    assert not track_d.PAUSE_GATE.is_paused()


# ---------------------------------------------------- adapter (real suites) -

def test_adapter_runs_real_suite_function(fresh_suites):
    gate = track_d.SoakGate.from_files()
    adapter = track_d.SoakAdapter(
        gate, readers_fn=lambda: [
            track_d.FakeReader("GemPCTwin serial 00 00"),
            track_d.FakeReader("ACS ACR1252 00 00")],
        sleep_fn=lambda s: None)
    fw = adapter.reader_info(track_d.FIRMWARE_READER_NEEDLE)
    acr = adapter.reader_info(track_d.ACR_READER_NEEDLE)
    row = adapter.run_test("soak_01_smoke", "select_mf", fw, acr, iteration=3)
    assert row["status"] == "PASS", row
    assert row["iteration"] == 3 and row["dur_ms"] >= 0
    assert row["firmware"]["sw"] == "0x6A82"
    # suite 10 runs on BOTH readers; different ATRs become a note, not FAIL
    row10 = adapter.run_test("soak_10_cross_compare", "identical_atr", fw,
                             acr)
    assert row10["status"] == "PASS"
    assert "acr" in row10


def test_adapter_refuses_unaudited_and_failed_tests(fresh_suites):
    gate = track_d.SoakGate.from_files()
    adapter = track_d.SoakAdapter(gate, readers_fn=lambda: [],
                                  sleep_fn=lambda s: None)
    refused = adapter.run_test("soak_02_globalplatform", "anything", {}, None)
    assert refused["status"] == "REFUSED"
    assert "no reviewed allowlist entry" in refused["error"]
    missing_fn = adapter.run_test("soak_01_smoke", "connect_and_atr", {}, None)
    assert missing_fn["status"] == "FAIL"  # reader_info {} -> no reader


# ------------------------------------------------- escape 0xD0 telemetry ----

def test_telemetry_esc_unsupported_is_honest_skip():
    tracker = track_d.TelemetryTracker(feature_fn=lambda conn: {})
    row = tracker.poll(track_d.FakeConn())
    assert row["status"] == "SKIP"
    assert row["escape_supported"] is False
    assert "ifdDriverOptions" in row["reason"] and "todo 14" in row["reason"]
    assert tracker.poll(track_d.FakeConn()).get("silent") is True


def test_telemetry_control_path_poll_and_violations():
    state = {"n": 0}

    def control(ioctl, data):
        assert data == [0xD0]
        state["n"] += 1
        n = state["n"]
        return track_d.diag_bytes(10 + n, 10 + n, 0, 0, 0, 1, 1000 + n)

    tracker = track_d.TelemetryTracker(feature_fn=lambda c: {19: 0x42})
    conn = track_d.FakeConn()
    conn.control = control
    r1 = tracker.poll(conn)
    assert r1["status"] == "PASS" and r1["escape_supported"] is True
    assert r1["apdu_tx_count"] == 11 and r1["uptime_ticks"] == 1001
    assert tracker.poll(conn)["status"] == "PASS"

    def regress(ioctl, data):
        return track_d.diag_bytes(1, 1, 0, 0, 1, 1, 5)
    conn.control = regress
    r3 = tracker.poll(conn)
    assert r3["status"] == "FAIL"
    assert len(r3["violations"]) == 4
    assert any("reinit_count changed" in v for v in r3["violations"])


# ----------------------------------------------------------- lane wiring ----

class FakeCtx:
    def __init__(self, dry_run=False):
        self.rows, self.anomalies, self.skips, self.events = [], [], [], []
        self.dry_run = dry_run
        self.pace_s = 0.0
        self._stop = threading.Event()

    def running(self):
        return not self._stop.is_set()

    def paused(self):
        return False

    def sleep(self, s):
        pass

    def row(self, **fields):
        self.rows.append(fields)
        return fields

    def skip(self, reason, **fields):
        return self.row(type="SKIP", status="SKIP", reason=reason, **fields)

    def anomaly(self, kind, **fields):
        self.anomalies.append((kind, fields))
        return fields

    def event(self, kind, **fields):
        self.events.append(kind)
        return fields


def test_lane_dry_run_emits_simulated_rows():
    ctx = FakeCtx(dry_run=True)
    track_d.run(ctx)
    kinds = [r["type"] for r in ctx.rows]
    assert "soak_gate" in kinds and "reader_gate" in kinds
    assert "soak_iter" in kinds and "telemetry" in kinds
    assert "atr_stability" in kinds
    gate_row = next(r for r in ctx.rows if r["type"] == "soak_gate")
    assert gate_row["tests"] == 29


def test_lane_mode_b_degrades_when_reader_gate_fails(monkeypatch):
    class NoGemaltoAdapter(track_d.SoakAdapter):
        def reader_names(self):
            return ["ACS ACR1252 00 00"]  # GemPCTwin missing

    monkeypatch.setattr(track_d, "SoakAdapter", NoGemaltoAdapter)
    ctx = FakeCtx()
    track_d.run(ctx)
    kinds = [k for k, _ in ctx.anomalies]
    assert "mode_b_reader_gate" in kinds
    skips = [r for r in ctx.rows if r["type"] == "SKIP"]
    assert len(skips) == 3
    assert any("atr stability" in r["reason"] for r in skips)
    assert not any(r["type"] == "soak_test" for r in ctx.rows)


def test_lane_soak_gate_refused_when_allowlist_missing(tmp_path,
                                                       monkeypatch):
    monkeypatch.setattr(track_d, "ALLOWLIST_PATH", tmp_path / "gone.json")
    ctx = FakeCtx()
    track_d.run(ctx)
    assert any(k == "soak_gate_refused" for k, _ in ctx.anomalies)
    assert ctx.rows and ctx.rows[-1]["type"] == "SKIP"


def test_register_alias_and_build_lane_shape():
    assert track_d.register is track_d.run
    spec = track_d.build_lane()
    assert spec.name == "track_d_soak"
    assert spec.window == "window2"
    assert spec.needs_pcscd is True
    assert spec.target is track_d.run


# ------------------------------------------------ stale pcsc context ----

def _stub_smartcard_pcsc(monkeypatch, readers_fn, ctx):
    import types

    base = types.ModuleType("smartcard")
    sys_mod = types.ModuleType("smartcard.System")
    sys_mod.readers = staticmethod(readers_fn)
    base.System = sys_mod
    ctx_mod = types.ModuleType("smartcard.pcsc.PCSCContext")
    ctx_mod.PCSCContext = ctx
    pcsc_mod = types.ModuleType("smartcard.pcsc")
    pcsc_mod.PCSCContext = ctx
    base.pcsc = pcsc_mod
    monkeypatch.setitem(sys.modules, "smartcard", base)
    monkeypatch.setitem(sys.modules, "smartcard.System", sys_mod)
    monkeypatch.setitem(sys.modules, "smartcard.pcsc", pcsc_mod)
    monkeypatch.setitem(sys.modules, "smartcard.pcsc.PCSCContext", ctx_mod)


def test_adapter_default_readers_renews_stale_pcsc_context(monkeypatch):
    # the soak lane resumes polling after every maintenance pcscd restart;
    # a stale singleton would fail the reader gate against a healthy daemon
    calls = {"n": 0, "renewed": 0}

    class Ctx:
        @staticmethod
        def renewContext():
            calls["renewed"] += 1

    def flaky():
        calls["n"] += 1
        if calls["n"] == 1:
            raise RuntimeError("ListReadersException: stale context")
        return ["GemPCTwin serial 00 00"]

    _stub_smartcard_pcsc(monkeypatch, flaky, Ctx)
    adapter = track_d.SoakAdapter(track_d.SoakGate(reviewed=[], fresh=None))
    assert adapter.reader_names() == ["GemPCTwin serial 00 00"]
    assert calls["renewed"] == 1


def test_adapter_default_readers_gives_up_after_one_renew(monkeypatch):
    calls = {"renewed": 0}

    class Ctx:
        @staticmethod
        def renewContext():
            calls["renewed"] += 1

    def always_dead():
        raise RuntimeError("EstablishContextException: pcscd down")

    _stub_smartcard_pcsc(monkeypatch, always_dead, Ctx)
    adapter = track_d.SoakAdapter(track_d.SoakGate(reviewed=[], fresh=None))
    with pytest.raises(RuntimeError, match="EstablishContextException"):
        adapter.reader_names()
    assert calls["renewed"] == 1
