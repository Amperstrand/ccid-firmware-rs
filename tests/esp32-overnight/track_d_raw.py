#!/usr/bin/env python3
"""Track D: raw GemPC Twin framing abuse (plan todo 14).

Direct serial abuse of the esp32-ccid GemPC Twin framing layer. After EVERY
abuse case a valid framed GetSlotStatus is sent and a well-formed
RDR_to_PC_SlotStatus with a fresh sequence number must come back (RESYNC
PROOF). Runs ONLY while holding the orchestrator's global pcscd-maintenance
lock (todo 5, Metis f — Track B and Track D soak are paused): pcscd is
stopped, the stick's port is opened raw (115200 8N2), the cases run, then
pcscd is restored per flash_and_test.sh:82-96 (stale comm socket + restart +
one retry) and readers() must show GemPCTwin AND ACR1252 again before the
lock is released. A device that stays silent after a case is EXPECTED
self-healing/reinit behavior (mfrc522_driver re-inits after 3 consecutive
failures): it is recorded as a read-timeout row + one retry + anomaly, and
the run continues — never a hard failure.

Frame format — crates/ccid-transport-serial/src/lib.rs:12-96:

    [SYNC 0x03][CTRL 0x06|0x15][CCID message][LRC]
    LRC = XOR of ALL preceding frame bytes (lib.rs:54-56).
    CCID message = 10-byte header (ccid-protocol types.rs:38) + dwLength
    payload; max frame 274 B = 2 + 271 (types.rs:39) + 1 (lib.rs:19).

The device ECHOES every accepted frame before responding (main.rs:277-280),
so the byte stream after a GetSlotStatus contains: our own request echoed
(type 0x65, matching seq, valid LRC), an optional NotifySlotChange
(0x50 xx, lib.rs:84-96), then the real SlotStatus (type 0x81). The detector
below keys on type 0x81 + fresh seq + valid LRC, which also neutralises the
echo-frame abuse case (a stale 0x81 echo never matches a later fresh seq).

Usage:
    python3 track_d_raw.py --selftest   # offline: factories + parsers only
Integration (todo-5 LANE API): register(ctx) — see overnight.py docstring.
"""

from __future__ import annotations

import argparse
import contextlib
import os
import subprocess
import sys
import threading
import time

# ------------------------------------------------- spec constants ----
# Mirrored from crates/ccid-transport-serial/src/lib.rs:12-96 and
# crates/ccid-protocol/src/types.rs — the source of truth, cited per use.

SYNC = 0x03  # lib.rs:12
CTRL_ACK = 0x06  # lib.rs:13
CTRL_NAK = 0x15  # lib.rs:14
CCID_HEADER_SIZE = 10  # types.rs:38
MAX_CCID_MESSAGE_LENGTH = 271  # types.rs:39
MAX_CCID_PAYLOAD = MAX_CCID_MESSAGE_LENGTH - CCID_HEADER_SIZE  # 261, lib.rs:18
PC_TO_RDR_GET_SLOT_STATUS = 0x65  # types.rs:6
PC_TO_RDR_XFR_BLOCK = 0x6F  # types.rs:7
RDR_TO_PC_SLOT_STATUS = 0x81  # types.rs:21

# Raw link parameters (AGENTS.md: GemPC Twin serial, 115200 8N2; read
# timeout 2 s per plan todo 14 — same shape switch_role.sh:38 opens).
SERIAL_BAUD = 115200
SERIAL_TIMEOUT_S = 2.0
QUIET_S = 0.4  # abuse-noise drain quiet window
NOISE_MAX_S = 4.0  # ...and its hard cap
RESYNC_TRIES = 3  # plan todo 14: resync proof within 3 tries
RESYNC_TIMEOUT_S = 3.0  # libccidtwin readTimeout is 3s (main.rs:284)

NAK_STORM_COUNT = 50  # plan todo 14: 50x bare NAK frames
GARBAGE_LEN = 4096  # plan todo 14: 4KB garbage

STICK_BY_ID = "/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-if00-port0"

# pcscd lifecycle — plan todo 14 + flash_and_test.sh:82-96 / switch_role.sh:66-71
STOP_CMD = ["sudo", "systemctl", "stop", "pcscd.socket", "pcscd.service"]
RM_COMM_CMD = ["sudo", "rm", "-f", "/var/run/pcscd/pcscd.comm"]
RESTART_CMD = ["sudo", "systemctl", "restart", "pcscd.socket", "pcscd.service"]
IS_ACTIVE_CMD = ["sudo", "systemctl", "is-active", "--quiet", "pcscd"]
EXPECTED_READERS = ("GemPCTwin", "ACR1252")


class _SystemClock:
    monotonic = staticmethod(time.monotonic)
    sleep = staticmethod(time.sleep)


_SYSTEM_CLOCK = _SystemClock()


# ------------------------------------------------------ FrameBuilder ----


class FrameBuilder:
    """Raw GemPC Twin frame construction, byte-compatible with
    crates/ccid-transport-serial/src/lib.rs:12-96."""

    @staticmethod
    def lrc(data) -> int:
        """XOR of all frame bytes preceding the LRC (lib.rs:54-56)."""
        acc = 0
        for b in data:
            acc ^= b
        return acc

    @staticmethod
    def ccid_header(
        msg_type: int,
        dw_length: int = 0,
        slot: int = 0,
        seq: int = 0,
        b_status: int = 0,
        b_error: int = 0,
    ) -> bytes:
        """10-byte CCID header (types.rs:38): type | dwLength LE32 | slot |
        seq | bStatus | bError | bRFU (shape per lib.rs test helper
        header_with_payload_len, lib.rs:245-258)."""
        return (
            bytes([msg_type])
            + int(dw_length).to_bytes(4, "little")
            + bytes([slot & 0xFF, seq & 0xFF, b_status & 0xFF, b_error & 0xFF, 0])
        )

    @staticmethod
    def frame(ccid_message: bytes) -> bytes:
        """[SYNC][CTRL_ACK][ccid message][LRC] — mirrors build_response_frame
        (lib.rs:58-74); identical wire shape for host->device requests."""
        body = bytes([SYNC, CTRL_ACK]) + bytes(ccid_message)
        return body + bytes([FrameBuilder.lrc(body)])

    # -- valid exchange -------------------------------------------------------

    @staticmethod
    def get_slot_status(seq: int) -> bytes:
        """Valid framed PC_to_RDR_GetSlotStatus: type 0x65, dwLength 0,
        slot 0, seq N (types.rs:6)."""
        return FrameBuilder.frame(
            FrameBuilder.ccid_header(PC_TO_RDR_GET_SLOT_STATUS, seq=seq)
        )

    # -- abuse case factories (plan todo 14) -----------------------------------

    @staticmethod
    def bad_lrc(seq: int) -> bytes:
        """Valid GetSlotStatus frame with the LRC byte corrupted — mirrors
        test_parser_invalid_lrc (lib.rs:324-343): device must answer
        FrameError::InvalidLrc and resync on the next valid frame."""
        good = bytearray(FrameBuilder.get_slot_status(seq))
        good[-1] ^= 0xFF
        return bytes(good)

    @staticmethod
    def oversized_dwlength() -> bytes:
        """Header declaring dwLength=0xFFFFFFFF and nothing else. The device
        parser fires Overflow the instant the 10-byte header completes
        (payload_len > MAX_CCID_PAYLOAD, lib.rs:134-144) — no LRC is ever
        read, so none is sent."""
        return bytes([SYNC, CTRL_ACK]) + FrameBuilder.ccid_header(
            PC_TO_RDR_GET_SLOT_STATUS, dw_length=0xFFFFFFFF
        )

    @staticmethod
    def truncated_header() -> bytes:
        """SYNC+CTRL+4 header bytes: CCID header never reaches 10 bytes, the
        parser parks mid-header emitting nothing (lib.rs:131-155, tests
        lib.rs:510-527)."""
        return bytes([SYNC, CTRL_ACK, PC_TO_RDR_GET_SLOT_STATUS, 0x01, 0x00, 0x00])

    @staticmethod
    def sync_byte_mid_payload() -> bytes:
        """Well-formed (valid-LRC) XfrBlock whose payload is SYNC bytes: the
        parser must treat mid-payload 0x03 as data, never resync mid-frame
        (lib.rs:157-165, test_sync_byte_in_payload_treated_as_data
        lib.rs:546-569)."""
        payload = bytes([SYNC]) * 8
        return FrameBuilder.frame(
            FrameBuilder.ccid_header(PC_TO_RDR_XFR_BLOCK, dw_length=len(payload))
            + payload
        )

    @staticmethod
    def nak_storm() -> bytes:
        """NAK_STORM_COUNT bare [SYNC, CTRL_NAK, LRC] frames — build_nak_frame
        (lib.rs:76-82); LRC(0x03,0x15)=0x16 per test_lrc_nak (lib.rs:261-263)."""
        single = bytes([SYNC, CTRL_NAK, FrameBuilder.lrc(bytes([SYNC, CTRL_NAK]))])
        return single * NAK_STORM_COUNT

    @staticmethod
    def garbage_4k(seed: int = 0xC0FFEE) -> bytes:
        """Deterministic 4KB garbage (LCG) — reproducible abuse bytes."""
        out = bytearray(GARBAGE_LEN)
        x = seed & 0xFFFFFFFF
        for i in range(GARBAGE_LEN):
            x = (1103515245 * x + 12345) & 0xFFFFFFFF
            out[i] = (x >> 16) & 0xFF
        return bytes(out)

    @staticmethod
    def echo_frame(seq: int) -> bytes:
        """Frame shaped like the device's OWN SlotStatus response (type 0x81,
        valid LRC). The device echoes every accepted frame (main.rs:277-280),
        so a stale echo of this frame must never satisfy a later resync
        proof — the fresh-seq requirement is the defense."""
        return FrameBuilder.frame(
            FrameBuilder.ccid_header(RDR_TO_PC_SLOT_STATUS, seq=seq)
        )


# ------------------------------------------------ SlotStatusDetector ----


class SlotStatusDetector:
    """Scans a noisy byte stream for one well-formed framed
    RDR_to_PC_SlotStatus: [SYNC][CTRL_ACK][type 0x81][10B header][payload]
    [LRC], LRC valid (lib.rs:166-173), seq == expected_seq. Everything else
    (garbage, NotifySlotChange 0x50, our echoed requests, stale 0x81 echoes)
    is skipped."""

    _PREFIX = bytes([SYNC, CTRL_ACK, RDR_TO_PC_SLOT_STATUS])

    def __init__(self, expected_seq: int):
        self.expected_seq = expected_seq
        self.buf = bytearray()
        self.result = None
        self.skipped_valid = 0  # well-formed frames with a stale seq

    def feed(self, data: bytes):
        """Append stream bytes; return the matching frame dict once (also in
        self.result), else None."""
        if self.result is not None:
            return self.result
        self.buf += bytes(data)
        return self._scan()

    def _scan(self):
        buf = self.buf
        i = 0
        while True:
            j = buf.find(self._PREFIX, i)
            if j < 0:
                break
            if len(buf) < j + 2 + CCID_HEADER_SIZE:
                break  # header incomplete — wait for more bytes
            header = buf[j + 2 : j + 2 + CCID_HEADER_SIZE]
            dw = int.from_bytes(header[1:5], "little")
            if dw > MAX_CCID_PAYLOAD:
                i = j + 1  # oversized lie — resume scan after this SYNC
                continue
            total = 2 + CCID_HEADER_SIZE + dw + 1
            if len(buf) < j + total:
                break  # payload+LRC incomplete — wait for more bytes
            frame = bytes(buf[j : j + total])
            if FrameBuilder.lrc(frame[:-1]) != frame[-1]:
                i = j + 1  # bad LRC — keep scanning (lib.rs:324-343 class)
                continue
            if header[6] != self.expected_seq:
                self.skipped_valid += 1
                i = j + total  # valid but stale (echo-frame abuse defense)
                continue
            self.result = {
                "seq": header[6],
                "slot": header[5],
                "dw_length": dw,
                "b_status": header[7],
                "b_error": header[8],
                "cmd_status": (header[7] >> 6) & 0x03,
                "icc_status": header[7] & 0x03,
                "frame_hex": frame.hex(),
            }
            del buf[: j + total]
            return self.result
        if len(buf) > 8192:
            del buf[:-4096]
        return None


# -------------------------------------------------------- RawSession ----


def _pyserial_factory(port: str):
    import serial  # lazy: tests inject fakes; no import-time port access

    return serial.Serial(
        port=port,
        baudrate=SERIAL_BAUD,
        bytesize=8,
        parity="N",
        stopbits=2,
        timeout=SERIAL_TIMEOUT_S,
    )


class RawSession:
    """Raw 115200 8N2 session on the stick's by-id port, read timeout 2 s.
    serial_factory is injectable so tests never touch a real port."""

    def __init__(
        self,
        port: str,
        *,
        serial_factory=None,
        clock=None,
        timeout_s: float = SERIAL_TIMEOUT_S,
    ):
        self.port = port
        self.timeout_s = float(timeout_s)
        self._factory = serial_factory or _pyserial_factory
        self._clock = clock or _SYSTEM_CLOCK
        self._transport = None

    def open(self) -> None:
        self._transport = self._factory(self.port)

    def close(self) -> None:
        transport, self._transport = self._transport, None
        if transport is not None:
            with contextlib.suppress(Exception):
                transport.close()

    def send_bytes(self, data: bytes) -> int:
        data = bytes(data)
        if self._transport is None:
            raise RuntimeError("RawSession not open")
        self._transport.write(data)
        self._transport.flush()
        return len(data)

    def read_until_quiet(
        self, quiet_s: float = QUIET_S, max_s: float = NOISE_MAX_S
    ) -> bytes:
        """Drain abuse noise: read until `quiet_s` of silence or `max_s`."""
        transport = self._transport
        if transport is None:
            raise RuntimeError("RawSession not open")
        buf = bytearray()
        start = last_rx = self._clock.monotonic()
        while True:
            data = transport.read(4096)
            now = self._clock.monotonic()
            if data:
                buf += data
                last_rx = now
                continue
            if now - last_rx >= quiet_s or now - start >= max_s:
                return bytes(buf)

    def await_slot_status(self, expected_seq: int, timeout_s: float = RESYNC_TIMEOUT_S):
        """Read until SlotStatusDetector matches seq or timeout; dict|None."""
        transport = self._transport
        if transport is None:
            raise RuntimeError("RawSession not open")
        detector = SlotStatusDetector(expected_seq)
        deadline = self._clock.monotonic() + float(timeout_s)
        while self._clock.monotonic() < deadline:
            data = transport.read(4096)
            if data and detector.feed(data) is not None:
                return detector.result
        return None


# -------------------------------------------------------- CaseRunner ----


class CaseRunner:
    """One abuse case = send abuse bytes, then prove resync with a valid
    GetSlotStatus -> well-formed SlotStatus (fresh seq) within RESYNC_TRIES.
    A silent device is EXPECTED self-healing: read-timeout row, ONE retry,
    anomaly, continue (plan todo 14)."""

    CASES = [
        ("bad_lrc", lambda seq: FrameBuilder.bad_lrc(seq)),
        ("oversized_dwlength", lambda seq: FrameBuilder.oversized_dwlength()),
        ("truncated_header", lambda seq: FrameBuilder.truncated_header()),
        ("sync_byte_mid_payload", lambda seq: FrameBuilder.sync_byte_mid_payload()),
        ("nak_storm", lambda seq: FrameBuilder.nak_storm()),
        ("garbage_4k", lambda seq: FrameBuilder.garbage_4k()),
        ("echo_frame", lambda seq: FrameBuilder.echo_frame(seq)),
    ]

    def __init__(
        self,
        session,
        *,
        emit,
        anomaly,
        diag_probe=None,
        running=None,
        clock=None,
        sleep_fn=None,
    ):
        self.session, self.emit, self.anomaly = session, emit, anomaly
        self.diag_probe = diag_probe
        self._running = running
        self._clock = clock or _SYSTEM_CLOCK
        self._sleep = sleep_fn or self._clock.sleep
        self._seq = 0

    def _next_seq(self) -> int:
        """CCID seq byte, incremented across EVERY case and resync attempt."""
        self._seq = (self._seq + 1) % 256
        return self._seq

    def _diag(self):
        if self.diag_probe is None:
            return None
        try:
            d = self.diag_probe()
        except Exception:
            return None
        if not isinstance(d, dict):
            return None
        value = d.get("reinit_count")
        return int(value) if isinstance(value, (int, float)) else None

    def _probe_slot_status(self):
        seq = self._next_seq()
        self.session.send_bytes(FrameBuilder.get_slot_status(seq))
        return self.session.await_slot_status(seq), seq

    def run_case(self, name: str, abuse: bytes) -> dict:
        diag_before = self._diag()
        sent = self.session.send_bytes(abuse)
        noise = self.session.read_until_quiet()
        self.emit(
            type="raw_case",
            case=name,
            status="SENT",
            bytes_sent=sent,
            noise_len=len(noise),
            noise_head=noise[:32].hex(),
        )
        frame, seq, attempts, retried = None, None, 0, False
        for attempts in range(1, RESYNC_TRIES + 1):
            frame, seq = self._probe_slot_status()
            if frame is not None:
                break
        if frame is None:
            self.emit(
                type="resync_proof",
                case=name,
                status="READ_TIMEOUT",
                tries=RESYNC_TRIES,
            )
            frame, seq = self._probe_slot_status()  # exactly ONE retry
            retried = True
            attempts = RESYNC_TRIES + 1
        diag = self._diag_fields(diag_before)
        if frame is not None:
            self.emit(
                type="resync_proof",
                case=name,
                status="PASS",
                seq=seq,
                attempts=attempts,
                retried=retried,
                b_status=frame["b_status"],
                cmd_status=frame["cmd_status"],
                icc_status=frame["icc_status"],
                **diag,
            )
            return {
                "case": name,
                "status": "PASS",
                "attempts": attempts,
                "retried": retried,
            }
        self.anomaly("resync_failed", case=name, tries=attempts)
        self.emit(type="resync_proof", case=name, status="FAIL", tries=attempts, **diag)
        return {"case": name, "status": "FAIL", "attempts": attempts}

    def _diag_fields(self, before) -> dict:
        after = self._diag()
        if before is None or after is None:
            return {}
        return {
            "reinit_before": before,
            "reinit_after": after,
            "reinit_delta": after - before,
        }

    def run_all(self) -> list:
        out = []
        for name, factory in self.CASES:
            if self._running is not None and not self._running():
                break
            out.append(self.run_case(name, factory(self._next_seq())))
        return out


# --------------------------------------------------- LockCoordinator ----


class LockUnavailable(Exception):
    """The orchestrator pcscd-maintenance lock could not be acquired."""


class LockCoordinator:
    """Bounded acquire of the orchestrator's pcscd-maintenance lock (todo-5
    PcscdMaintenanceLock protocol: hold(who) is a BLOCKING context manager).
    On timeout the late acquisition self-releases inside the helper thread,
    so a contended lock is never wedged by us. NEVER run unlocked."""

    def __init__(
        self, lock, *, who: str = "track_d_raw", acquire_timeout_s: float = 90.0
    ):
        self.lock, self.who = lock, who
        self.acquire_timeout_s = float(acquire_timeout_s)
        self._cm = None

    def acquire(self) -> None:
        if self.lock is None:
            raise LockUnavailable(
                "no pcscd-maintenance lock provided (todo-5 ctx exposes none)"
            )
        hold = getattr(self.lock, "hold", None)
        if not callable(hold):
            raise LockUnavailable("lock does not implement the hold(who) protocol")
        cm = hold(self.who)
        entered, decision = threading.Event(), threading.Event()
        state = {"owned": False, "error": None}

        def _worker():
            try:
                cm.__enter__()
            except BaseException as e:  # noqa: BLE001 — recorded, re-raised below
                state["error"] = e
                entered.set()
                return
            entered.set()
            decision.wait()
            if not state["owned"]:  # given up on: undo the late acquisition
                with contextlib.suppress(BaseException):
                    cm.__exit__(None, None, None)

        threading.Thread(
            target=_worker, name=f"{self.who}-lock-acquire", daemon=True
        ).start()
        if not entered.wait(self.acquire_timeout_s):
            decision.set()
            raise LockUnavailable(
                f"pcscd-maintenance lock held elsewhere; not acquired within "
                f"{self.acquire_timeout_s:.1f}s"
            )
        if state["error"] is not None:
            raise LockUnavailable(f"lock.hold() raised: {state['error']!r}")
        state["owned"] = True
        decision.set()
        self._cm = cm

    def release(self) -> None:
        cm, self._cm = self._cm, None
        if cm is not None:
            with contextlib.suppress(BaseException):
                cm.__exit__(None, None, None)

    def __enter__(self):
        self.acquire()
        return self

    def __exit__(self, *exc):
        self.release()
        return False


# ----------------------------------------------------------- PcscdOps ----


class PcscdOps:
    """pcscd stop/restore around the raw window. stop = plan todo 14 stop
    sequence (+ stale comm socket clear); restore mirrors flash_and_test.sh
    start_pcscd (:82-96: rm comm + restart + 8s wait + is-active) with the
    switch_role.sh:66-71 once-retry, then readers() must list BOTH expected
    readers again. run/sleep/readers injectable; calls/sleeps recorded."""

    def __init__(
        self,
        *,
        run=None,
        sleep=None,
        readers=None,
        boot_wait_s: float = 8.0,
        stop_wait_s: float = 1.0,
    ):
        self.calls, self.sleeps = [], []
        self._run_ext = run
        self._sleep_ext = sleep or time.sleep
        self._readers_ext = readers
        self.boot_wait_s, self.stop_wait_s = float(boot_wait_s), float(stop_wait_s)

    def _run(self, cmd):
        self.calls.append(list(cmd))
        if self._run_ext is not None:
            return self._run_ext(cmd)
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
        return proc.returncode, proc.stdout

    def _sleep(self, s: float) -> None:
        self.sleeps.append(s)
        self._sleep_ext(s)

    def stop(self) -> None:
        self._run(STOP_CMD)
        self._run(RM_COMM_CMD)
        self._sleep(self.stop_wait_s)  # flash_and_test.sh:57 port-release wait

    def restore(self) -> dict:
        """flash_and_test.sh:82-96 + one retry; returns ok/attempts/readers."""
        attempts, active = 0, False
        for attempts in (1, 2):
            self._run(RM_COMM_CMD)  # :85 stale comm socket
            self._run(RESTART_CMD)  # :86
            self._sleep(self.boot_wait_s)  # :87-88 boot + pcscd init
            rc, _ = self._run(IS_ACTIVE_CMD)  # :90
            active = rc == 0
            if active:
                break
        readers = self._readers() if active else []
        missing = [
            name for name in EXPECTED_READERS if not any(name in r for r in readers)
        ]
        ok = active and not missing
        reason = (
            ""
            if ok
            else (
                "pcscd not active after retry"
                if not active
                else f"readers missing: {missing}"
            )
        )
        return {
            "ok": ok,
            "attempts": attempts,
            "pcscd_active": active,
            "readers": readers,
            "reason": reason,
        }

    def _readers(self) -> list:
        if self._readers_ext is not None:
            try:
                return [str(r) for r in self._readers_ext()]
            except Exception:
                return []
        try:
            from smartcard.System import readers  # lazy, pcscd-gated
        except Exception:
            return []
        try:
            return [str(r) for r in readers()]
        except Exception:
            return []


# ---------------------------------------------------- integration ----

_UNSET = object()


def default_port() -> str:
    """Stable by-id path (switch_role.sh:20 / reader.conf:16 convention)."""
    return os.environ.get("TRACK_D_RAW_PORT") or STICK_BY_ID


def _lock_from_ctx(ctx):
    """Duck-typed lock discovery on the todo-5 context: pcscd_lock attribute
    or locks['pcscd']. Absent -> None -> honest SKIP (never run unlocked)."""
    lock = getattr(ctx, "pcscd_lock", None)
    if lock is not None:
        return lock
    locks = getattr(ctx, "locks", None)
    if isinstance(locks, dict):
        return locks.get("pcscd")
    return None


def _default_diag_probe():
    """Optional reinit telemetry via todo 13's escape parser (track_d module).
    Graceful when absent."""
    try:
        import track_d  # noqa: F401 — todo 13 sibling, same directory
    except ImportError:
        return None
    for name in ("escape_diagnostics", "read_diagnostics", "query_diagnostics"):
        fn = getattr(track_d, name, None)
        if callable(fn):
            return fn
    return None


def register(
    ctx,
    *,
    lock=None,
    port=None,
    session_factory=None,
    pcscd=None,
    diag_probe=_UNSET,
    clock=None,
    sleep_fn=None,
    acquire_timeout_s: float = 90.0,
):
    """Track entry (todo-5 LANE API): the full raw abuse window under the
    global pcscd-maintenance lock — acquire -> stop pcscd -> run cases ->
    restore pcscd + verify readers -> release. Every hardware touchpoint is
    injectable; defaults are the real ones."""
    if getattr(ctx, "dry_run", False):
        return ctx.skip("dry-run: raw framing abuse needs the real stick")
    coord = LockCoordinator(
        lock if lock is not None else _lock_from_ctx(ctx),
        acquire_timeout_s=acquire_timeout_s,
    )
    try:
        coord.acquire()
    except LockUnavailable as e:
        return ctx.skip(
            f"pcscd-maintenance lock unavailable — never run unlocked ({e})"
        )
    ops = pcscd or PcscdOps(sleep=sleep_fn)
    try:
        ops.stop()
        ctx.event("pcscd_stopped_for_raw_window")
        session = None
        try:
            session = (
                session_factory()
                if session_factory
                else RawSession(port or default_port(), clock=clock)
            )
            if hasattr(session, "open"):
                session.open()
        except OSError as e:
            ctx.anomaly("raw_session_open_failed", error=repr(e))
            session = None
        if session is not None:
            probe = _default_diag_probe() if diag_probe is _UNSET else diag_probe
            runner = CaseRunner(
                session,
                emit=ctx.row,
                anomaly=ctx.anomaly,
                diag_probe=probe,
                running=getattr(ctx, "running", None),
                clock=clock,
                sleep_fn=sleep_fn,
            )
            summaries = runner.run_all()
            passed = sum(1 for s in summaries if s["status"] == "PASS")
            ctx.row(
                type="raw_window",
                status="PASS" if passed == len(summaries) else "PARTIAL",
                cases=len(summaries),
                passed=passed,
            )
            if hasattr(session, "close"):
                session.close()
    finally:
        res = ops.restore()
        if res["ok"]:
            ctx.row(
                type="pcscd_restore",
                status="PASS",
                attempts=res["attempts"],
                readers=res["readers"],
            )
        else:
            ctx.anomaly("pcscd_restore_failed", **res)
        coord.release()
    return None


def build_lane():
    """Optional todo-5 hook (overnight.load_track_specs protocol):
    build_lane() -> LaneSpec. Returns None when the orchestrator module is
    not importable (standalone/selftest use)."""
    try:
        import overnight  # present when run under the orchestrator process
    except ImportError:
        return None
    return overnight.LaneSpec(
        "track_d_raw", register, window="window2", needs_pcscd=True
    )


# ------------------------------------------------------------ selftest ----


def _selftest() -> int:
    fb = FrameBuilder
    checks = []

    def check(name, cond):
        checks.append((name, bool(cond)))

    # 1. LRC correctness of every factory frame (spec: lib.rs:54-56)
    for name, frame in (
        ("get_slot_status", fb.get_slot_status(5)),
        ("sync_byte_mid_payload", fb.sync_byte_mid_payload()),
        ("echo_frame", fb.echo_frame(5)),
    ):
        check(
            f"lrc:{name}",
            frame[0] == SYNC
            and frame[1] == CTRL_ACK
            and fb.lrc(frame[:-1]) == frame[-1],
        )
    bad = fb.bad_lrc(5)
    check(
        "lrc:bad_lrc_is_invalid",
        fb.lrc(bad[:-1]) != bad[-1] and bad[:-1] == fb.get_slot_status(5)[:-1],
    )
    # 2. GetSlotStatus request shape (types.rs:6)
    req = fb.get_slot_status(5)
    check(
        "shape:get_slot_status",
        len(req) == 2 + CCID_HEADER_SIZE + 1
        and req[2] == 0x65
        and req[3:7] == b"\x00\x00\x00\x00"
        and req[7] == 0
        and req[8] == 5,
    )
    # 3. oversized dwLength lie (lib.rs:134-144)
    over = fb.oversized_dwlength()
    check(
        "shape:oversized",
        len(over) == 2 + CCID_HEADER_SIZE
        and int.from_bytes(over[3:7], "little") == 0xFFFFFFFF
        and int.from_bytes(over[3:7], "little") > MAX_CCID_PAYLOAD,
    )
    # 4. truncated header < 10 CCID bytes (lib.rs:131-155)
    trunc = fb.truncated_header()
    check(
        "shape:truncated",
        trunc[0:2] == bytes([SYNC, CTRL_ACK]) and len(trunc) - 2 < CCID_HEADER_SIZE,
    )
    # 5. NAK storm (lib.rs:76-82, 261-263)
    storm = fb.nak_storm()
    single = bytes([SYNC, CTRL_NAK, fb.lrc(bytes([SYNC, CTRL_NAK]))])
    check(
        "shape:nak_storm",
        storm == single * NAK_STORM_COUNT and single == b"\x03\x15\x16",
    )
    # 6. garbage deterministic 4KB
    check(
        "shape:garbage",
        len(fb.garbage_4k()) == GARBAGE_LEN and fb.garbage_4k() == fb.garbage_4k(),
    )
    # 7. echo frame is device-response shaped (main.rs:277-280)
    echo = fb.echo_frame(9)
    check(
        "shape:echo_frame",
        echo[2] == RDR_TO_PC_SLOT_STATUS
        and echo[8] == 9
        and fb.lrc(echo[:-1]) == echo[-1],
    )
    # 8. detector: valid response found inside echo + notify noise
    det = SlotStatusDetector(expected_seq=5)
    stream = (
        fb.get_slot_status(5)
        + b"\x50\x03"
        + fb.echo_frame(4)
        + fb.frame(fb.ccid_header(RDR_TO_PC_SLOT_STATUS, seq=5, b_status=0x41))
    )
    got = det.feed(stream)
    check(
        "detector:accepts_valid",
        got is not None
        and got["seq"] == 5
        and got["b_status"] == 0x41
        and got["icc_status"] == 1
        and got["cmd_status"] == 1,
    )
    # 9. detector: bad LRC skipped, wrong seq skipped
    det2 = SlotStatusDetector(expected_seq=1)
    corrupt = bytearray(fb.frame(fb.ccid_header(RDR_TO_PC_SLOT_STATUS, seq=1)))
    corrupt[-1] ^= 0xFF
    check(
        "detector:rejects_bad_lrc",
        det2.feed(bytes(corrupt)) is None and det2.result is None,
    )
    det3 = SlotStatusDetector(expected_seq=2)
    det3.feed(fb.echo_frame(7))
    check("detector:rejects_wrong_seq", det3.result is None and det3.skipped_valid == 1)

    failed = [n for n, ok in checks if not ok]
    for name, ok in checks:
        print(f"  {'ok  ' if ok else 'FAIL'} {name}")
    if failed:
        print(f"selftest FAIL ({len(failed)}/{len(checks)} checks failed)")
        return 1
    print(f"selftest PASS ({len(checks)} checks)")
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description="Track D raw GemPC framing abuse (plan todo 14)"
    )
    ap.add_argument(
        "--selftest",
        action="store_true",
        help="offline self-test: frame factories + parsers only",
    )
    args = ap.parse_args(argv)
    if args.selftest:
        return _selftest()
    ap.error(
        "standalone mode is --selftest only; live runs go through "
        "register(ctx) under the orchestrator pcscd-maintenance lock"
    )
    return 2


if __name__ == "__main__":
    sys.exit(main())
