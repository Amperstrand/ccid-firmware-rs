"""Offline tests for track_d_raw (plan todo 14).

No serial, no pcscd, no sudo: every hardware touchpoint (serial transport,
pcscd commands, lock, ctx) is a fake. Frame bytes are verified against the
SPEC constants mirrored from crates/ccid-transport-serial/src/lib.rs:12-96
and crates/ccid-protocol/src/types.rs, not against golden byte strings.
"""

from __future__ import annotations

import sys
import threading
from collections import deque
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import track_d_raw as tdr  # noqa: E402


# ------------------------------------------------------------- fakes ----


class FakeCtx:
    """Duck-typed overnight.PhaseContext subset (todo-5 LANE API)."""

    def __init__(self, running=True, dry_run=False):
        self.rows, self.anomalies, self.events = [], [], []
        self._running, self.dry_run = running, dry_run

    def running(self):
        return self._running

    def row(self, **fields):
        self.rows.append(fields)
        return fields

    def skip(self, reason, **fields):
        return self.row(type="SKIP", status="SKIP", reason=reason, **fields)

    def anomaly(self, kind, **fields):
        self.anomalies.append({"kind": kind, **fields})
        return self.anomalies[-1]

    def event(self, kind, **fields):
        self.events.append({"kind": kind, **fields})
        return self.events[-1]


class FakeClock:
    def __init__(self):
        self.t = 1000.0

    def monotonic(self):
        return self.t

    def sleep(self, s):
        self.t += max(0.0, float(s))


class RecordingLock:
    """Duck-typed todo-5 PcscdMaintenanceLock: hold(who) context manager."""

    def __init__(self, busy=False):
        self.acquired, self.released = [], []
        self._inner = threading.Lock()
        if busy:
            self._inner.acquire()

    def hold(self, who):
        outer = self

        class _CM:
            def __enter__(self):
                if not outer._inner.acquire(timeout=5.0):
                    raise RuntimeError("lock busy")
                outer.acquired.append(who)
                return self

            def __exit__(self, *exc):
                outer.released.append(who)
                outer._inner.release()
                return False

        return _CM()


class FakeSerial:
    """Device emulator: echoes every accepted 03 06 frame (main.rs:277-280),
    answers GetSlotStatus with a valid SlotStatus carrying the request seq,
    pumps the fake clock when idle so bounded waits terminate."""

    def __init__(self, clock, *, respond=True, noise=b""):
        self.clock = clock
        self.respond, self.noise = respond, noise
        self.writes = []
        self.rx = deque()
        self.closed = False

    # device behavior -------------------------------------------------------
    def write(self, data):
        data = bytes(data)
        self.writes.append(data)
        if self.noise:
            self.rx.append(self.noise)
        self._scan_frames(data)

    def _scan_frames(self, data):
        i = 0
        while True:
            j = data.find(b"\x03\x06", i)
            if j < 0 or len(data) < j + 12:
                return
            header = data[j + 2 : j + 12]
            dw = int.from_bytes(header[1:5], "little")
            total = 2 + 10 + dw + 1
            if len(data) < j + total:
                return
            frame = data[j : j + total]
            if tdr.FrameBuilder.lrc(frame[:-1]) != frame[-1]:
                i = j + 1
                continue
            self.rx.append(frame)  # device echoes every accepted frame
            if header[0] == tdr.PC_TO_RDR_GET_SLOT_STATUS and self.respond:
                resp = tdr.FrameBuilder.frame(
                    tdr.FrameBuilder.ccid_header(
                        tdr.RDR_TO_PC_SLOT_STATUS, seq=header[6], b_status=0x00
                    )
                )
                self.rx.append(resp)
            i = j + total

    # transport behavior ----------------------------------------------------
    def read(self, n=4096):
        if self.rx:
            return self.rx.popleft()[:n]
        self.clock.sleep(0.05)  # idle pump: bounded loops terminate on fake time
        return b""

    def flush(self):
        pass

    def close(self):
        self.closed = True


def make_pcscd_ops(
    clock=None, *, active_first_try=True, readers=None
) -> "tdr.PcscdOps":
    """A PcscdOps with recording built in (ops.calls / ops.sleeps)."""
    is_active_count = [0]

    def run(cmd):
        if cmd[:3] == ["sudo", "systemctl", "is-active"]:
            is_active_count[0] += 1
            ok = active_first_try or is_active_count[0] > 1
            return (0 if ok else 3), ""
        return 0, ""

    def sleep(s):
        if clock:
            clock.sleep(s)

    names = (
        readers
        if readers is not None
        else ["GemPCTwin serial 00 00", "ACS ACR1252 01 00"]
    )
    return tdr.PcscdOps(run=run, sleep=sleep, readers=lambda: list(names))


def make_runner(fake_serial, ctx=None, diag=None):
    ctx = ctx or FakeCtx()
    clock = fake_serial.clock
    session = tdr.RawSession(
        "port-unused", serial_factory=lambda _: fake_serial, clock=clock
    )
    session.open()
    runner = tdr.CaseRunner(
        session,
        emit=ctx.row,
        anomaly=ctx.anomaly,
        diag_probe=diag,
        clock=clock,
        sleep_fn=clock.sleep,
    )
    return ctx, runner


def never_called_session():
    raise AssertionError("register must not construct a session in this state")


# ------------------------------------------------ FrameBuilder: LRC ----


def test_every_factory_frame_lrc_correct_and_bad_lrc_is_not():
    seq = 7
    frames = {
        "get_slot_status": tdr.FrameBuilder.get_slot_status(seq),
        # oversized_dwlength is deliberately LRC-less: the device parser
        # fires Overflow at header completion, before any LRC byte (lib.rs:134-144)
        "sync_byte_mid_payload": tdr.FrameBuilder.sync_byte_mid_payload(),
        "echo_frame": tdr.FrameBuilder.echo_frame(seq),
    }
    for name, frame in frames.items():
        assert frame[0] == tdr.SYNC and frame[1] == tdr.CTRL_ACK, name
        assert tdr.FrameBuilder.lrc(frame[:-1]) == frame[-1], name
    storm = tdr.FrameBuilder.nak_storm()
    assert len(storm) == 3 * tdr.NAK_STORM_COUNT
    for k in range(tdr.NAK_STORM_COUNT):
        chunk = storm[3 * k : 3 * k + 3]
        assert chunk == b"\x03\x15\x16"  # build_nak_frame, lib.rs:76-82
        assert tdr.FrameBuilder.lrc(chunk[:-1]) == chunk[-1]
    # negative case: valid frame, deliberately corrupted LRC
    bad = tdr.FrameBuilder.bad_lrc(seq)
    assert tdr.FrameBuilder.lrc(bad[:-1]) != bad[-1]
    # garbage: right size, deterministic, not accidentally LRC-clean frames
    g = tdr.FrameBuilder.garbage_4k()
    assert len(g) == 4096 and g == tdr.FrameBuilder.garbage_4k()


# --------------------------------------------- FrameBuilder: shapes ----


def test_get_slot_status_request_shape():
    frame = tdr.FrameBuilder.get_slot_status(seq=9)
    ccid = frame[2:-1]
    assert len(frame) == 2 + tdr.CCID_HEADER_SIZE + 1  # dwLength 0
    assert ccid[0] == 0x65  # PC_to_RDR_GetSlotStatus, types.rs:6
    assert ccid[1:5] == b"\x00\x00\x00\x00"  # dwLength = 0
    assert ccid[5] == 0  # slot
    assert ccid[6] == 9  # seq
    assert tdr.FrameBuilder.get_slot_status(9) != tdr.FrameBuilder.get_slot_status(10)


def test_abuse_case_shapes():
    over = tdr.FrameBuilder.oversized_dwlength()
    assert over[2:7] == b"\x65\xff\xff\xff\xff"  # type 0x65, dwLength 0xFFFFFFFF
    assert len(over) == 2 + tdr.CCID_HEADER_SIZE  # header lie only, no LRC
    assert int.from_bytes(over[3:7], "little") == 0xFFFFFFFF
    assert int.from_bytes(over[3:7], "little") > tdr.MAX_CCID_PAYLOAD

    trunc = tdr.FrameBuilder.truncated_header()
    assert len(trunc) < 2 + tdr.CCID_HEADER_SIZE + 1
    assert trunc[0:2] == b"\x03\x06"

    mid = tdr.FrameBuilder.sync_byte_mid_payload()
    ccid = mid[2:-1]
    assert ccid[0] == 0x6F  # XfrBlock carries the payload
    assert tdr.SYNC in ccid[10:]  # sync byte mid-payload is data, lib.rs:157-165
    assert int.from_bytes(ccid[1:5], "little") == len(ccid) - 10

    echo = tdr.FrameBuilder.echo_frame(seq=4)
    assert echo[2] == tdr.RDR_TO_PC_SLOT_STATUS and echo[8] == 4  # device-shaped


# --------------------------------------------- SlotStatusDetector ----


def build_slot_status_frame(seq, b_status=0x00):
    return tdr.FrameBuilder.frame(
        tdr.FrameBuilder.ccid_header(
            tdr.RDR_TO_PC_SLOT_STATUS, seq=seq, b_status=b_status
        )
    )


def test_detector_accepts_valid_frame_in_noise():
    d = tdr.SlotStatusDetector(expected_seq=3)
    stream = (
        b"\xde\xad"
        + b"\x50\x03"
        + tdr.FrameBuilder.get_slot_status(3)
        + build_slot_status_frame(3, b_status=0x41)
    )
    got = d.feed(stream)
    assert got is not None and got["seq"] == 3
    assert got["b_status"] == 0x41
    assert got["icc_status"] == 1 and got["cmd_status"] == 1  # soaklib split


def test_detector_rejects_bad_lrc_wrong_seq_and_request_echo():
    # 1. corrupted-LRC candidate is skipped, scanning continues
    d = tdr.SlotStatusDetector(expected_seq=1)
    bad = bytearray(build_slot_status_frame(1))
    bad[-1] ^= 0xFF
    assert d.feed(bytes(bad)) is None and d.result is None
    assert d.feed(build_slot_status_frame(1))["seq"] == 1  # still finds the good one
    # 2. valid frame with the WRONG seq is skipped (echo-frame abuse defense)
    d2 = tdr.SlotStatusDetector(expected_seq=2)
    assert d2.feed(build_slot_status_frame(5)) is None
    assert d2.result is None and d2.skipped_valid == 1
    # 3. our own request echo (type 0x65) never matches a 0x81 detector
    d3 = tdr.SlotStatusDetector(expected_seq=2)
    assert d3.feed(tdr.FrameBuilder.get_slot_status(2)) is None
    assert d3.result is None


def test_detector_finds_frame_split_across_feeds():
    d = tdr.SlotStatusDetector(expected_seq=6)
    frame = build_slot_status_frame(6)
    for i in range(len(frame)):
        if d.feed(frame[i : i + 1]):
            break
    assert d.result is not None and d.result["seq"] == 6


# ------------------------------------------------ CaseRunner ----


def test_case_sequencing_and_resync_proof_seq_increments():
    fs = FakeSerial(FakeClock())
    ctx, runner = make_runner(fs)
    summary = runner.run_all()
    assert len(summary) == len(tdr.CaseRunner.CASES)
    case_rows = [r for r in ctx.rows if r.get("type") == "raw_case"]
    resync_rows = [r for r in ctx.rows if r.get("type") == "resync_proof"]
    assert len(case_rows) == len(tdr.CaseRunner.CASES)
    assert len(resync_rows) == len(tdr.CaseRunner.CASES)
    assert all(r["status"] == "PASS" for r in resync_rows)
    # every abuse payload was actually sent on the wire
    joined = b"".join(fs.writes)
    assert tdr.FrameBuilder.nak_storm() in joined
    assert tdr.FrameBuilder.garbage_4k() in joined
    # request seqs strictly increase across the whole run
    seqs = [
        w[8]
        for w in fs.writes
        if len(w) >= 13
        and w[0:2] == b"\x03\x06"
        and w[2] == 0x65
        and tdr.FrameBuilder.lrc(w[:-1]) == w[-1]
    ]
    assert seqs == sorted(seqs) and len(set(seqs)) == len(seqs)
    # each resync proof answered on the FIRST try against the emulator
    assert all(r["attempts"] == 1 for r in resync_rows)


def test_no_response_case_timeout_single_retry_then_anomaly_and_continue():
    fs = FakeSerial(FakeClock(), respond=False)  # device never answers
    ctx, runner = make_runner(fs)
    summary = runner.run_all()
    # policy fires for EVERY case and the runner still continues through all
    assert len(summary) == len(tdr.CaseRunner.CASES)
    assert all(s["status"] == "FAIL" for s in summary)
    timeout_rows = [r for r in ctx.rows if r.get("status") == "READ_TIMEOUT"]
    assert len(timeout_rows) == len(tdr.CaseRunner.CASES)
    assert len([a for a in ctx.anomalies if a["kind"] == "resync_failed"]) == len(
        tdr.CaseRunner.CASES
    )
    # retry policy: 3 tries + exactly ONE extra retry per case
    reqs = [
        w
        for w in fs.writes
        if len(w) >= 13
        and w[:3] == b"\x03\x06\x65"
        and tdr.FrameBuilder.lrc(w[:-1]) == w[-1]
    ]
    assert len(reqs) == len(tdr.CaseRunner.CASES) * (tdr.RESYNC_TRIES + 1)
    # later cases still ran (case rows exist for every case)
    assert len([r for r in ctx.rows if r.get("type") == "raw_case"]) == len(
        tdr.CaseRunner.CASES
    )


def test_diag_probe_records_reinit_delta():
    calls = iter([{"reinit_count": 2}, {"reinit_count": 5}])

    def probe():
        return next(calls)

    fs = FakeSerial(FakeClock())
    ctx, runner = make_runner(fs, diag=probe)
    runner.run_case("probe_case", b"\x03\x15\x16")
    rows = [r for r in ctx.rows if r.get("case") == "probe_case"]
    assert any(r.get("reinit_delta") == 3 for r in rows)


# ------------------------------------------------ LockCoordinator ----


def test_lock_unavailable_skip_and_never_touches_hardware():
    ctx, ops = FakeCtx(), make_pcscd_ops()
    tdr.register(ctx, lock=None, session_factory=never_called_session, pcscd=ops)
    assert len(ctx.rows) == 1 and ctx.rows[0]["type"] == "SKIP"
    assert "never run unlocked" in ctx.rows[0]["reason"]
    assert not ops.calls and not ctx.anomalies


def test_lock_contention_times_out_to_skip():
    lock = RecordingLock(busy=True)  # another holder (e.g. role gate)
    ctx, ops = FakeCtx(), make_pcscd_ops()
    tdr.register(
        ctx,
        lock=lock,
        pcscd=ops,
        acquire_timeout_s=0.2,
        session_factory=never_called_session,
    )
    assert ctx.rows and ctx.rows[0]["type"] == "SKIP"
    assert ops.calls == [] and lock.acquired == []


# ------------------------------------------------ PcscdOps ----


def test_restore_retries_once_and_verifies_readers():
    ops = make_pcscd_ops(active_first_try=False)
    res = ops.restore()
    assert res["ok"] and res["attempts"] == 2
    restarts = [c for c in ops.calls if c[:3] == ["sudo", "systemctl", "restart"]]
    assert len(restarts) == 2  # flash_and_test.sh:82-96 + switch_role retry
    assert any(c[:2] == ["sudo", "rm"] for c in ops.calls)  # stale comm socket


def test_restore_fails_when_reader_missing():
    ops = make_pcscd_ops(readers=["GemPCTwin serial 00 00"])  # ACR1252 gone
    res = ops.restore()
    assert res["ok"] is False and "ACR1252" in res["reason"]


# ------------------------------------------------ register (full) ----


def test_register_full_sequence_under_lock():
    clock = FakeClock()
    fs = FakeSerial(clock, noise=b"\x50\x03")
    lock = RecordingLock()
    ops = make_pcscd_ops(clock)
    ctx = FakeCtx()
    tdr.register(
        ctx,
        lock=lock,
        pcscd=ops,
        session_factory=lambda: tdr.RawSession(
            "port-unused", serial_factory=lambda _: fs, clock=clock
        ),
        clock=clock,
        sleep_fn=clock.sleep,
    )
    # lock acquired BEFORE any pcscd command, released exactly once after restore
    assert lock.acquired == ["track_d_raw"] and lock.released == ["track_d_raw"]
    stop = ["sudo", "systemctl", "stop", "pcscd.socket", "pcscd.service"]
    assert ops.calls[0] == stop  # stop is the first pcscd command
    assert ops.calls[-1][:3] == ["sudo", "systemctl", "is-active"]
    assert ops.sleeps  # boot wait honored (flash_and_test.sh:87-88)
    assert fs.closed
    restore_rows = [r for r in ctx.rows if r.get("type") == "pcscd_restore"]
    assert len(restore_rows) == 1 and restore_rows[0]["status"] == "PASS"
    assert any(
        r.get("type") == "resync_proof" and r["status"] == "PASS" for r in ctx.rows
    )
    assert not ctx.anomalies


def test_register_session_open_failure_restores_and_releases():
    clock = FakeClock()

    def broken_factory():
        raise OSError("port held by pcscd")

    lock, ops = RecordingLock(), make_pcscd_ops(clock)
    ctx = FakeCtx()
    tdr.register(
        ctx,
        lock=lock,
        pcscd=ops,
        session_factory=broken_factory,
        clock=clock,
        sleep_fn=clock.sleep,
    )
    kinds = [a["kind"] for a in ctx.anomalies]
    assert "raw_session_open_failed" in kinds
    assert lock.released == ["track_d_raw"]  # restore + release still ran
    assert any(c[:3] == ["sudo", "systemctl", "restart"] for c in ops.calls)


def test_register_dry_run_skips_without_hardware():
    ctx, ops, lock = FakeCtx(dry_run=True), make_pcscd_ops(), RecordingLock()
    tdr.register(ctx, lock=lock, session_factory=never_called_session, pcscd=ops)
    assert ctx.rows[0]["type"] == "SKIP" and ops.calls == [] and not lock.acquired


def test_selftest_entrypoint_exits_zero(capsys):
    assert tdr.main(["--selftest"]) == 0
    out = capsys.readouterr().out
    assert "selftest PASS" in out


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


def test_pcscd_ops_readers_renews_stale_context(monkeypatch):
    # restore() verifies readers in the SAME process that stopped pcscd:
    # without renewal the stale singleton fabricates a restore failure
    calls = {"n": 0, "renewed": 0}

    class Ctx:
        @staticmethod
        def renewContext():
            calls["renewed"] += 1

    def flaky():
        calls["n"] += 1
        if calls["n"] == 1:
            raise RuntimeError("ListReadersException: stale context")
        return ["GemPCTwin serial 00 00", "ACS ACR1252 00 00"]

    _stub_smartcard_pcsc(monkeypatch, flaky, Ctx)
    ops = tdr.PcscdOps()
    assert ops._readers() == ["GemPCTwin serial 00 00", "ACS ACR1252 00 00"]
    assert calls["renewed"] == 1


def test_pcscd_ops_readers_empty_after_failed_renew(monkeypatch):
    calls = {"renewed": 0}

    class Ctx:
        @staticmethod
        def renewContext():
            calls["renewed"] += 1

    def always_dead():
        raise RuntimeError("EstablishContextException: pcscd down")

    _stub_smartcard_pcsc(monkeypatch, always_dead, Ctx)
    ops = tdr.PcscdOps()
    assert ops._readers() == []
    assert calls["renewed"] == 1
