#!/usr/bin/env python3
"""No-card GemPC Twin conformance + fuzz battery for the bench readers.

Runs the identical host-side command battery against BOTH serial CCID
readers (nucula pn7160-ccid and m5stick MFRC522 firmware), compares the
card-absent responses structurally, then fuzzes the serial framing and
proves resync after every malformed input. Finishes with pcscd-restart
and GetSlotStatus soaks.

Reader ports are resolved dynamically from stable /dev/serial/by-id
identities (tests/hardware/serial_ports.py) — never hardcoded ttyUSBx,
which renumbers on USB re-enumeration. Override with NUCULA_PORT /
M5STICK_PORT env vars.

Direct serial access: pcscd MUST be stopped (it holds the ports).
The script stops/starts it itself. The labgrid places (nucula-c3,
m5stick) are acquired before any port is touched and released on
exit — a failed acquire aborts before stopping pcscd.

Usage:
    python3 tests/hardware/nfc/conformance_battery.py [--quick]
"""

import argparse
import random
import shutil
import subprocess
import sys
import time
from pathlib import Path

import serial

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from serial_ports import resolve  # noqa: E402

SYNC = 0x03
CTRL_ACK = 0x06
CTRL_NAK = 0x15

MSG_GET_SLOT_STATUS = 0x65
MSG_ICC_POWER_ON = 0x62
MSG_ICC_POWER_OFF = 0x63
MSG_GET_PARAMETERS = 0x61
MSG_XFR_BLOCK = 0x6F
MSG_ESCAPE = 0x6B

READERS = ("nucula", "m5stick")


def lrc(data: bytes) -> int:
    x = 0
    for b in data:
        x ^= b
    return x


def frame(msg_type: int, seq: int, data: bytes = b"", ctrl: int = CTRL_ACK,
          lrc_override: int | None = None) -> bytes:
    header = bytes([msg_type, len(data) & 0xFF, (len(data) >> 8) & 0xFF,
                    (len(data) >> 16) & 0xFF, (len(data) >> 24) & 0xFF,
                    0, seq, 0, 0, 0])
    body = bytes([SYNC, ctrl]) + header + data
    return body + bytes([lrc(body) if lrc_override is None else lrc_override])


def parse_frames(buf: bytes):
    """Structural parse: (type, seq, bStatus, bError, payload) per frame."""
    out = []
    i = 0
    while i < len(buf) - 12:
        if buf[i] != SYNC:
            i += 1
            continue
        f = buf[i:]
        mt = f[2]
        dlen = int.from_bytes(f[3:7], "little")
        total = 13 + dlen
        if total > len(f):
            i += 1
            continue
        if lrc(f[:total - 1]) != f[total - 1]:
            i += 1
            continue
        out.append((mt, f[8], f[9], f[10], f[12:12 + dlen]))
        i += total
    return out


class Reader:
    def __init__(self, name: str, port: str):
        self.name = name
        self.port = port
        # GemPC-Twin serial is 115200 8N2 — at the 8N1 default the
        # m5stick's UART sees framing errors and answers NOTHING (not
        # even the echo), so the battery silently tests a dead port
        # (found 2026-10-20 while instrumenting #330). The nucula CDC
        # is 8N1 and tolerates the 2-stop framing either way.
        self.s = serial.Serial(port=None, baudrate=115200, timeout=0.2,
                               stopbits=2)
        self.s.port = port
        self.s.dtr = False
        self.s.rts = False
        self.s.open()
        # AGENTS.md DTR-latch rule: pyserial asserts DTR at open; on the
        # nucula CDC that latches boot:0x5 on the next reset. Deassert
        # both lines immediately (Codex PR 107 P1). On a PTY (the broker
        # surface, /dev/nucula-ccid) there are no modem lines — the
        # ioctls fail with ENOTTY and DTR/RTS are a no-op by construction,
        # so the deassert is skipped rather than fatal (#305).
        try:
            self.s.setDTR(False)
            self.s.setRTS(False)
        except OSError:
            pass
        self.seq = 0
        # Dead-port verdict classification (#431): True while the serial
        # port is unusable (last reopen failed). Readers in this state
        # contribute SKIPs (bench fault), never toward PASS or FAIL.
        self.dead = False
        # Late-response accumulation (issue #114, Codex on #107): a
        # response arriving after the read window used to sit in the OS
        # buffer until the NEXT exchange discarded it via
        # reset_input_buffer — recoveries inside the retry budget read
        # as "NO RECOVERY". RDR frames parsed from the pre-drain and
        # the read window are stashed keyed by their seq byte; the
        # owning exchange (or the next one, for late arrivals) finds
        # them there.
        self.pending: dict[int, list] = {}

    def _stash_frames(self, buf: bytes) -> None:
        for f in parse_frames(buf):
            if f[0] >= 0x80:
                self.pending.setdefault(f[1], []).append(f)

    def _collect_response(self, seq: int) -> list:
        """Pop the response for `seq`; when unanswered, surface late
        responses stashed for earlier seqs (oldest first) instead of
        reporting no response — a valid late frame proves the reader
        recovered within the budget."""
        own = self.pending.pop(seq, None)
        if own:
            return own
        late: list = []
        for stale in list(self.pending):
            late.extend(self.pending.pop(stale))
        return late

    def close(self):
        try:
            self.s.close()
        except Exception:
            pass

    def reopen(self, quick: bool = False) -> bool:
        """Recover from a USB-CDC drop (the nucula re-enumerates on reset).

        A mid-battery disconnect raises SerialException from read/write;
        one clean close+open cycle is the documented bench remedy.

        #431 verdict classification: a FAILED reopen marks the reader
        dead (``self.dead``) — callers must count affected cases as SKIP
        (bench fault), never toward a PASS. ``quick=True`` does one
        immediate attempt without the 1 s re-enumeration waits, for the
        per-case cadence on a reader already known dead.
        """
        self.dead = True  # presumed dead until a fresh open succeeds
        try:
            self.s.close()
        except Exception:
            pass
        attempts, delay = (1, 0.0) if quick else (5, 1.0)
        for _ in range(attempts):
            if delay:
                time.sleep(delay)
            try:
                # Codex P1 on #331: preserve the constructor's serial
                # settings on reopen — a bare Serial(port, 115200)
                # resets to 8N1 and the m5stick GemPC UART (8N2) goes
                # framing-dead, exactly the bug the safe-open fixed.
                s = serial.Serial()
                s.port = self.port
                s.baudrate = 115200
                s.timeout = 0.2
                s.stopbits = self.s.stopbits if self.s else 2
                s.dtr = False
                s.rts = False
                s.open()
                self.s = s
                self.dead = False
                return True
            except serial.SerialException:
                continue
        return False

    def quiet_bytes(self, window_s: float = 2.0) -> bytes:
        """Collect any UNSOLICITED bytes with no frame sent (silence check).

        Clean-wire enforcement (issues #131/#133): post-handshake, the
        GemPC wires must deliver zero bytes until the host speaks. The
        m5stick twin of the nucula's test_wire_silence_after_banner.

        #339 (mirrors the fixed nucula twin, Codex #145 P2): silence can
        never be PROVEN on a dropped port. A failed reopen RAISES — a
        dead reader is a bench hardware fault, not a quiet wire (the
        old `break` returned b"" collected-so-far and PASSed the check
        on a dead reader). A successful reopen restarts the FULL quiet
        window so the post-reconnect observation is as long as the
        original, not whatever the reconnect left over.
        """
        got = bytearray()
        deadline = time.time() + window_s
        # #361 review P2: a flapping port (drop → reopen ok → drop …)
        # must not extend the quiet window forever — the bench
        # launchers' `timeout` kill bypasses cleanup while labgrid
        # places are held and pcscd is stopped. Bounded reopens only
        # (no wall-clock multiple: a legitimately slow reopen cycle is
        # not a fault).
        reopens = 0
        MAX_QUIET_REOPENS = 3
        self.s.timeout = 0.1
        while time.time() < deadline:
            try:
                chunk = self.s.read(8192)
            except serial.SerialException:
                if not self.reopen():
                    raise AssertionError(
                        f"{self.name}: serial port dropped during the "
                        "quiet window and reopen failed — bench hardware "
                        "fault, not wire silence (#339)"
                    ) from None
                reopens += 1
                if reopens > MAX_QUIET_REOPENS:
                    raise AssertionError(
                        f"{self.name}: {reopens - 1} reopens inside one "
                        "quiet window — flapping port, bench hardware "
                        "fault (#361 review P2)"
                    )
                # Codex #145 P2 twin: a successful reconnect restarts
                # the FULL quiet window.
                deadline = time.time() + window_s
                continue
            if chunk:
                got.extend(chunk)
        return bytes(got)

    def exchange(self, msg_type: int, data: bytes = b"", raw: bytes | None = None,
                 settle: float = 0.15) -> list:
        """Send (raw bytes if given, else a frame); return parsed RDR frames.

        Reads until the line goes idle (two consecutive empty reads AFTER
        `settle` seconds have elapsed) or the hard cap (`settle` + 1.5 s).
        `settle` is the reader's legitimate slow-path budget — error paths
        take up to ~0.8 s (presence retries + status LED logging on the
        m5stick). Counting idle reads before `settle` is what produced the
        1/87 truncated-header false flag (issue #89): a ~0.5 s response
        arriving after ~0.3 s of silence was declared missing.

        A transient port drop yields [] after a reopen attempt instead
        of killing the whole battery run with an unhandled SerialException.
        A FAILED reopen leaves the reader flagged dead (#431): the []
        then means "port unusable, bench fault" — callers classify by
        ``r.dead``, not by the empty list alone.
        """
        try:
            return self._exchange_once(msg_type, data, raw, settle)
        except serial.SerialException:
            if not self.reopen():  # nucula CDC readiness quirk cycle
                return []          # reader marked dead — SKIP, not FAIL
            try:
                return self._exchange_once(msg_type, data, raw, settle)
            except serial.SerialException:
                # Degrade to no-response, never kill the battery run: the
                # case counts as failed, the remaining cases still run.
                return []

    def _exchange_once(self, msg_type: int, data: bytes, raw: bytes | None,
                       settle: float) -> list:
        self.seq = (self.seq % 255) + 1
        seq = self.seq
        payload = raw if raw is not None else frame(msg_type, seq, data)
        # Pre-drain: bytes that arrived after the previous window closed
        # are the late responses the pending buffer exists for — they
        # must be read BEFORE reset_input_buffer discards them.
        try:
            if self.s.in_waiting:
                self.s.timeout = 0.02
                self._stash_frames(self.s.read(max(self.s.in_waiting, 1)))
        except serial.SerialException:
            pass
        try:
            self.s.reset_input_buffer()
            self.s.write(payload)
        except serial.SerialException:
            if not self.reopen():
                return []
            try:
                self.s.write(payload)
            except serial.SerialException:
                return []
        self.dead = False  # the frame left on a working port
        self.s.timeout = 0.15
        buf = b""
        idle = 0
        t0 = time.time()
        while time.time() - t0 < settle + 1.5:
            try:
                chunk = self.s.read(8192)
            except serial.SerialException:
                if not self.reopen():
                    return []
                continue
            if chunk:
                buf += chunk
                idle = 0
            elif time.time() - t0 >= settle:
                idle += 1
            if idle >= 2:
                break
        self._stash_frames(buf)
        return self._collect_response(seq)


def pcscd_stop():
    subprocess.run(["systemctl", "stop", "pcscd.socket", "pcscd.service"],
                   capture_output=True)
    subprocess.run(["pkill", "-9", "-x", "pcscd"], capture_output=True)
    time.sleep(1.5)


def pcscd_start():
    subprocess.run(["systemctl", "start", "pcscd.socket", "pcscd.service"],
                   capture_output=True)
    time.sleep(1.5)


# Bench disturbance doctrine (tollgate-lab #15/#16): every pcscd restart
# on the shared bench goes through pcscd-ctl (B32 disturbance lock +
# waiting reader sessions + journaled reason). Naked systemctl restarts
# bounced reader sessions bench-wide (ntag424-delta lane, 2026-10-23:
# pcscd cycled 80x in 3h, ACR1252 dropped from pcsc). The direct-serial
# phases below still need a hard stop (pcscd must stay DOWN while the
# probe owns the ports) — pcscd-ctl has no stop; only the restart cycles
# route through it.
PCSCD_CTL = shutil.which("pcscd-ctl")


def pcscd_restart_coordinated(reason: str) -> bool:
    """Restart pcscd the sanctioned way when pcscd-ctl exists (bench);
    fall back to the naked stop+start pair on hosts without it (CI).
    Returns False if the coordinated restart FAILED — the caller must
    count that as a failed cycle and NOT probe: the daemon is then
    whatever it was before (likely still running with the old reader
    set), and probing it would false-pass the soak."""
    if PCSCD_CTL:
        r = subprocess.run(
            [PCSCD_CTL, "restart",
             "--project", "ccid-conformance-battery",
             "--reason", reason],
            capture_output=True, text=True, timeout=120)
        if r.returncode != 0:
            print(f"  pcscd-ctl restart FAILED (rc={r.returncode}): "
                  f"{(r.stderr or r.stdout).strip()[:200]}")
            return False
        return True
    pcscd_stop()
    pcscd_start()
    return True


# Labgrid bench doctrine (AGENTS.md): every HIL session acquires the
# DUT places before touching hardware — concurrent sessions cannot
# race a device. Places from tests/hardware/labgrid/
# exporter-ai-legion-nfc.yaml.
LABGRID_PLACES = {"nucula": "nucula-c3", "m5stick": "m5stick"}


def labgrid_acquire(places: list[str]) -> list[str]:
    """Acquire each place; returns the places actually held (partial on
    failure — the caller must release exactly those)."""
    acquired = []
    for place in places:
        r = subprocess.run(["labgrid-client", "-p", place, "acquire"],
                           capture_output=True, text=True)
        if r.returncode != 0:
            reason = (r.stderr or r.stdout).strip()
            print(f"labgrid: acquire {place} FAILED: {reason}")
            return acquired
        print(f"labgrid: acquired {place}")
        acquired.append(place)
    return acquired


def labgrid_release(places: list[str]) -> None:
    for place in places:
        subprocess.run(["labgrid-client", "-p", place, "release"],
                       capture_output=True)


# ---------------------------------------------------------------- battery

def conformance_battery(readers: dict[str, Reader],
                        skips: dict[str, int] | None = None) -> bool:
    """Same commands through every reader; compare card-state semantics.

    Card-dependent cases (IccPowerOn, XfrBlock) adapt to each reader's
    detected ICC state: the bench card lives on the m5stick coil (issue
    #89 run, 2026-10-09: PowerOn legitimately SUCCEEDS there — activation
    retries landed with #88), so a hardcoded no-card expectation fails
    against correct firmware. Those cases are verified per-reader only
    and marked "n/a (card-relative)" — their outcomes depend on physical
    coupling, not on protocol divergence between readers.

    #431: a case whose reader port is dead (reopen failed) is recorded
    as SKIP — a bench fault, excluded from PASS/FAIL counting and from
    the cross-reader comparison — and tallied into `skips`.
    """
    ok = True

    def icc_state(r: Reader) -> int:
        resp = r.exchange(MSG_GET_SLOT_STATUS)
        if resp and resp[0][0] == 0x81:
            return resp[0][2] & 0x03  # CCID bmICCStatus: 0=present+active 1=present+inactive 2=no ICC
        return -1

    icc = {rname: icc_state(r) for rname, r in readers.items()}

    def power_on_expect(resp, icc_bits):
        if not resp:
            return False
        if icc_bits == 0:  # no card: must fail cleanly
            return (resp[0][2] & 0xC0) != 0
        # card coupled: activation success (ATR) or marginal-coupling
        # failure are both valid firmware outcomes
        return True

    def xfr_expect(resp, icc_bits):
        if not resp:
            return False
        if icc_bits == 0:  # no card: ICC_NOT_ACTIVE / failed status
            return (resp[0][2] & 0xC0) != 0
        return True  # card answer relayed or activation-state error — both valid

    cases = [
        ("Escape get-version", MSG_ESCAPE, bytes([0x02]),
         lambda r, i: r and r[0][0] == 0x83 and len(r[0][4]) > 0),
        ("Escape sync-enable", MSG_ESCAPE, bytes([0x01, 0x01, 0x01]),
         lambda r, i: r and r[0][0] == 0x83),
        # Card-state aware (2026-10-23): with no ICC on the coil the
        # CORRECT answer is bmICCStatus=2 (no ICC); the old expectation
        # only accepted present-states and failed honest card-absent
        # benches (the icc probe feeds each reader's state as i).
        ("GetSlotStatus", MSG_GET_SLOT_STATUS, b"",
         lambda r, i: r and r[0][0] == 0x81 and (
             (r[0][2] & 0x03) in (0x00, 0x01) if i in (0, 1)
             else (r[0][2] & 0x03) == 0x02)),
        ("IccPowerOn", MSG_ICC_POWER_ON, b"", power_on_expect),
        ("IccPowerOff", MSG_ICC_POWER_OFF, b"",
         lambda r, i: bool(r)),
        ("GetParameters", MSG_GET_PARAMETERS, b"",
         lambda r, i: bool(r)),
        ("ResetParameters", 0x6D, b"",
         lambda r, i: bool(r)),
        ("XfrBlock", MSG_XFR_BLOCK, bytes([0x00, 0xA4, 0x04, 0x00, 0x00]),
         xfr_expect),
    ]
    results: dict[str, dict[str, object]] = {}
    for name, mt, data, expect in cases:
        for rname, r in readers.items():
            resp = r.exchange(mt, data)
            if not resp and r.dead:
                # #431: dead port — bench fault. SKIP the case for this
                # reader; never FAIL honest firmware on a missing board
                # and never count the case as executed.
                results.setdefault(name, {})[rname] = (None, "SKIP (dead port)")
                if skips is not None:
                    skips[rname] = skips.get(rname, 0) + 1
                continue
            passed = False
            detail = "no response"
            if resp:
                t, seq, st, err, payload = resp[0]
                detail = f"type=0x{t:02X} bStatus=0x{st:02X} err=0x{err:02X} len={len(payload)}"
                try:
                    passed = bool(expect(resp, icc[rname]))
                except Exception:
                    passed = False
            results.setdefault(name, {})[rname] = (passed, detail)
            if not passed:
                ok = False

    run_names = list(readers.keys())
    print(f"\nICC state: {icc} (0=present+active 1=present+inactive 2=no ICC)")
    header = "   ".join(f"{rn:>30s}" for rn in run_names)
    print(f"{'case':28s} {header}   verdict")
    for name, per in results.items():
        cells = []
        for rn in run_names:
            p, d = per[rn]
            state = "SKIP" if p is None else ("PASS" if p else "FAIL")
            cells.append(f"{d[:28]:>28s} {state}")
        # structural agreement: same response type AND same command-status
        # class (bits 6-7). Card-dependent cases are per-reader relative
        # (activation outcome depends on physical coupling — the bench card
        # lives on the m5stick coil) and never participate in the
        # reader-vs-reader comparison. #431: dead-port readers are SKIPped
        # and excluded here; agreement is judged among executed readers.
        sigs = []
        for rn in run_names:
            _p, d = per[rn]
            if _p is None:
                continue
            if d == "no response":
                sigs.append("none")
            else:
                t, st = d.split()[0], d.split()[1]
                sigs.append(f"{t}|{(int(st.split('=')[1], 16) >> 6) & 3}")
        if not sigs:
            verdict = "SKIP (all dead)"
        else:
            agree = len(set(sigs)) == 1
            if name in ("IccPowerOn", "XfrBlock"):
                verdict = "n/a (card-relative)"
            else:
                verdict = "AGREE" if agree else "DIVERGE"
        row = "   ".join(f"{c:>34s}" for c in cells)
        print(f"{name:28s} {row}   {verdict}")
        if verdict == "DIVERGE":
            ok = False
    return ok


# ------------------------------------------------------------------- fuzz

def fuzz_battery(readers: dict[str, Reader], rounds: int,
                 skips: dict[str, int] | None = None) -> bool:
    """Malformed frames must never wedge the reader: every case ends with a
    valid GetSlotStatus answered correctly.

    #431 verdict counting: a dead serial port (reopen failed) is a bench
    fault — the affected cases are SKIPped and tallied into `skips`,
    never counted toward a PASS. A wedge is reserved for the real
    failure class: port back, but the protocol does not resync.
    """
    ok = True
    rng = random.Random(20261009)
    # Wire-silence check (issues #131/#133): no frames sent for 2 s —
    # the wires must be SILENT (clean-wire by construction: ceiling
    # before the driver claim; any bytes here are a regression or a
    # reintroduced unsolicited log line).
    print("\nwire silence (2 s quiet window, no frames sent):")
    for rname, r in readers.items():
        try:
            quiet = r.quiet_bytes(2.0)
        except AssertionError as e:
            # #339/#361 twin of the #431 rule: silence can never be
            # PROVEN on a dead/flapping port — SKIP the check for this
            # reader (never PASS it), never abort the whole battery.
            print(f"  {rname}: SKIP — {e}")
            if skips is not None:
                skips[rname] = skips.get(rname, 0) + 1
            continue
        state = "PASS" if quiet == b"" else f"FAIL ({len(quiet)}B: {quiet[:24]!r})"
        print(f"  {rname}: {state}")
        if quiet:
            ok = False

    print(f"\nfuzz ({rounds} randomized rounds + fixed cases):")
    wedges = {n: 0 for n in readers}
    cases: list[tuple[str, bytes]] = [
        ("bad LRC", frame(MSG_GET_SLOT_STATUS, 1, lrc_override=0xFF)),
        ("truncated header", bytes([SYNC, CTRL_ACK, 0x65, 0x01])),
        ("no SYNC garbage", bytes(rng.randbytes(24))),
        ("SYNC + garbage", bytes([SYNC]) + bytes(rng.randbytes(20))),
        ("oversized dwLength", bytes([SYNC, CTRL_ACK, 0x6F, 0xFF, 0xFF, 0xFF, 0x00,
                                      0x00, 0x01, 0x00, 0x00, 0x00]) + bytes([0x00])),
        ("lone SYNC", bytes([SYNC])),
        ("NAK ctrl frame", bytes([SYNC, CTRL_NAK])),
    ]
    for rnd in range(rounds):
        n = rng.randint(1, 48)
        body = bytes(rng.randbytes(n))
        if rng.random() < 0.7:
            body = bytes([SYNC]) + body
        cases.append((f"random-{rnd}", body))

    for rname, r in readers.items():
        reader_skips = 0
        for name, raw in cases:
            if r.dead:
                # #431: reader known dead — one cheap probe per case
                # (no 5x1s reopen cycle); a still-dead port SKIPs the
                # case, it never silently counts toward the executed
                # total. A reopened port resumes normal testing.
                if not r.reopen(quick=True):
                    reader_skips += 1
                    continue
            r.seq = (r.seq % 255) + 1
            try:
                r.s.reset_input_buffer()
                r.s.write(raw)
            except serial.SerialException:
                if not r.reopen():
                    reader_skips += 1
                    print(f"  SKIP {rname} case {name!r}: port dead, "
                          "reopen failed — bench fault (#431)")
                    continue
                # Port back: prove protocol resync (same disarm-then-
                # probe discipline as the normal path — T=1 fuzz
                # payloads can leave the endpoint armed, issue #101).
                r.exchange(0x6D, settle=0.2)
                answered = False
                for _ in range(3):
                    resp = r.exchange(MSG_GET_SLOT_STATUS, settle=0.6)
                    if resp and resp[0][0] == 0x81:
                        answered = True
                        break
                if not answered:
                    if r.dead:
                        reader_skips += 1
                        print(f"  SKIP {rname} after {name}: port died "
                              "again during resync probes (#431)")
                    else:
                        wedges[rname] += 1
                        ok = False
                        print(f"  WEDGE {rname} after {name}: no GetSlotStatus "
                              "resync within 3 probes after reopen")
                continue
            time.sleep(0.05)
            # Resync proof: a valid GetSlotStatus must be answered within a
            # bounded window. The FIRST probe after a truncated header is
            # legitimately eaten — the parser is mid-header and consumes the
            # probe's bytes as header continuation until the ~100 ms read-idle
            # reset clears it (issue #89 recovery probe: both readers answer
            # within 0.8 s, second probe always clean). A single-probe check
            # misread that recovery as a wedge; poll up to 3 probes.
            # settle: the m5stick's presence poll + status LED logging take
            # ~500ms — a fast probe misreports a slow response as a wedge.
            # ResetParameters first: T=1 fuzz payloads can arm the endpoint
            # (issue #101) — disarm so the raw GetSlotStatus works.
            r.exchange(0x6D, settle=0.2)
            answered = None
            for attempt in range(3):
                resp = r.exchange(MSG_GET_SLOT_STATUS, settle=0.6)
                if resp and resp[0][0] == 0x81:
                    answered = attempt
                    break
            if answered is None:
                if r.dead:
                    # port died during the probes and never came back —
                    # bench fault, not a firmware wedge (#431)
                    reader_skips += 1
                    print(f"  SKIP {rname} after {name}: port died during "
                          "resync probes — bench fault (#431)")
                else:
                    wedges[rname] += 1
                    ok = False
                    latency = recovery_latency(r, raw)
                    print(f"  WEDGE {rname} after {name}: no valid GetSlotStatus in 3 probes"
                          + (f"; recovered after {latency:.2f}s" if latency is not None
                             else "; NO RECOVERY within 5s"))
        if reader_skips and skips is not None:
            skips[rname] = skips.get(rname, 0) + reader_skips
        print(f"  {rname}: {len(cases)} fuzz cases, {wedges[rname]} wedges, "
              f"{reader_skips} skipped (dead port)")
    return ok


def recovery_latency(r: Reader, raw: bytes, budget: float = 5.0) -> float | None:
    """#89 instrumentation: map how long a wedged reader takes to recover.

    Re-sends the SAME malformed case (keeping the reader in the wedged
    state), then polls GetSlotStatus every 200 ms until a valid response
    — the latency distribution distinguishes 'slow path' from 'real
    stall until next-frame resync'.
    """
    t0 = time.time()
    try:
        r.s.write(raw)
    except serial.SerialException:
        r.reopen()
        return None
    while time.time() - t0 < budget:
        time.sleep(0.2)
        resp = r.exchange(MSG_GET_SLOT_STATUS, settle=0.2)
        if resp and resp[0][0] == 0x81:
            return time.time() - t0
    return None


# ------------------------------------------------------------------ soaks

def pcscd_restart_soak(readers: dict[str, Reader], count: int) -> bool:
    print(f"\npcscd restart soak ({count}x):")
    for r in readers.values():
        r.close()
    # Reader-name drift lesson (2026-10-23): match each configured
    # reader by ANY of its historical bench names, not one hardcoded
    # string — the m5stick conf was renamed 'M5stick GemPC (MFRC522)'
    # while this probe still hunted only 'GemPCTwin' (10/10 false FAIL).
    NAME_PATTERNS = {
        "nucula": ("Nucula",),
        "m5stick": ("GemPCTwin", "M5stick GemPC", "M5stick"),
    }
    run_names = list(readers.keys())
    probe = (
        "from smartcard.System import readers as R; "
        "names = [str(r) for r in R()]; "
        + " ".join(
            "print(any({} for n in names));".format(
                " or ".join(f"'{p}' in n" for p in NAME_PATTERNS[rn])
            )
            for rn in run_names
        )
    )
    ok = True
    for i in range(count):
        # one sanctioned restart per cycle: B32 disturbance lock, reader
        # sessions waited out, journaled reason. A FAILED restart counts
        # as a failed cycle and skips the probe — never false-pass on a
        # stale daemon (Codex P2 review, 2026-10-24).
        if not pcscd_restart_coordinated(f"restart-soak cycle {i}"):
            ok = False
            continue
        # sys.executable, NOT PATH python3: in agent/venv environments
        # PATH python3 lacks pyscard and the probe fails SILENTLY (empty
        # stdout maps to all-False — 10/10 false FAIL, 2026-10-23).
        out = subprocess.run(
            [sys.executable, "-c", probe],
            capture_output=True, text=True, timeout=30)
        flags = [ln.strip() == "True" for ln in out.stdout.splitlines()]
        states = dict(zip(run_names, flags)) if len(flags) == len(run_names) else {
            n: False for n in run_names
        }
        if not all(states.values()):
            ok = False
            print(f"  restart {i}: {states} — FAIL")
            # the known CDC-state remedy: one clean open/close cycle
            if "nucula" in states and not states["nucula"]:
                try:
                    s = serial.Serial(resolve("nucula"), 115200, timeout=0.5)
                    s.close()
                except Exception:
                    pass
    print(f"  {'all restarts clean' if ok else 'some restarts needed the CDC remedy'}")
    return ok


def slot_status_soak(readers: dict[str, Reader], count: int,
                     skips: dict[str, int] | None = None) -> bool:
    print(f"\nGetSlotStatus soak ({count}x per reader):")
    ok = True
    for rname, r in readers.items():
        errs = 0
        dead_skips = 0
        first_icc = None
        t0 = time.time()
        for i in range(count):
            resp = r.exchange(MSG_GET_SLOT_STATUS, settle=0.01)
            # per-iteration verdict stays LOCAL (#431): the old code
            # overwrote the function-level `ok`, so a misbehaving or
            # dead reader followed by a clean reader FALSE-PASSED the
            # soak (its errors were silently erased).
            resp_ok = bool(resp and resp[0][0] == 0x81)
            if resp_ok:
                icc = resp[0][2] & 7
                if first_icc is None:
                    first_icc = icc
                resp_ok = icc == first_icc
            if not resp_ok and r.dead:
                dead_skips += 1  # dead port: bench fault -> SKIP (#431)
            elif not resp_ok:
                errs += 1
        dt = time.time() - t0
        rate = count / dt if dt else 0
        print(f"  {rname}: {count - errs - dead_skips}/{count} clean "
              f"({rate:.0f}/s), {errs} errors, {dead_skips} skipped (dead port)")
        if dead_skips and skips is not None:
            skips[rname] = skips.get(rname, 0) + dead_skips
        if errs:
            ok = False
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true", help="shrink fuzz/soak rounds")
    ap.add_argument(
        "--readers",
        default=",".join(READERS),
        help="comma-separated subset to run (lane-held places block the default)",
    )
    args = ap.parse_args()
    readers_to_run = tuple(r.strip() for r in args.readers.split(",") if r.strip())
    unknown = [r for r in readers_to_run if r not in READERS]
    if unknown:
        ap.error(f"unknown readers {unknown} (known: {list(READERS)})")

    # Acquire the bench places BEFORE stopping pcscd or opening the
    # DUT ports (Codex on #107): the battery owns the hardware for the
    # whole run and must not race sibling sessions.
    places = [LABGRID_PLACES[name] for name in readers_to_run]
    acquired = labgrid_acquire(places)
    if len(acquired) != len(places):
        labgrid_release(acquired)
        sys.exit(2)
    try:
        pcscd_stop()
        readers: dict[str, Reader] = {}
        # #431 dead-port ledger: cases skipped because a reader's serial
        # port was dead (bench fault). They never count toward PASS; the
        # final verdict says "PASS with SKIPS" so a dead reader can never
        # read as a clean run.
        skips: dict[str, int] = {}
        try:
            for name in readers_to_run:
                readers[name] = Reader(name, resolve(name))
            ok1 = conformance_battery(readers, skips)
            ok2 = fuzz_battery(readers, 20 if args.quick else 80, skips)
            ok3 = pcscd_restart_soak(readers, 3 if args.quick else 10)
            # the restart soak leaves pcscd RUNNING (it holds the ports);
            # stop it and reopen our direct connections
            pcscd_stop()
            for name in list(readers):
                try:
                    readers[name].close()
                except Exception:
                    pass
                readers[name] = Reader(name, resolve(name))
            ok4 = slot_status_soak(readers, 100 if args.quick else 1000, skips)
        finally:
            for r in readers.values():
                r.close()
            pcscd_start()
    finally:
        labgrid_release(acquired)

    verdict = all([ok1, ok2, ok3, ok4])
    if verdict and any(skips.values()):
        print(f"\nBATTERY VERDICT: PASS with SKIPS — dead-port bench faults "
              f"excluded from counting, rerun on a healthy bench "
              f"(skipped: {skips}, #431)")
    else:
        print(f"\nBATTERY VERDICT: {'PASS' if verdict else 'FAIL'}")
    sys.exit(0 if verdict else 1)


if __name__ == "__main__":
    main()
