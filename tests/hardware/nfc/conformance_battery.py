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
        # both lines immediately (Codex PR 107 P1).
        self.s.setDTR(False)
        self.s.setRTS(False)
        self.seq = 0
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

    def reopen(self) -> bool:
        """Recover from a USB-CDC drop (the nucula re-enumerates on reset).

        A mid-battery disconnect raises SerialException from read/write;
        one clean close+open cycle is the documented bench remedy.
        """
        try:
            self.s.close()
        except Exception:
            pass
        for _ in range(5):
            time.sleep(1.0)
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
                return True
            except serial.SerialException:
                continue
        return False

    def quiet_bytes(self, window_s: float = 2.0) -> bytes:
        """Collect any UNSOLICITED bytes with no frame sent (silence check).

        Clean-wire enforcement (issues #131/#133): post-handshake, the
        GemPC wires must deliver zero bytes until the host speaks. The
        m5stick twin of the nucula's test_wire_silence_after_banner.
        """
        got = bytearray()
        t0 = time.time()
        self.s.timeout = 0.1
        while time.time() - t0 < window_s:
            try:
                chunk = self.s.read(8192)
            except serial.SerialException:
                if not self.reopen():
                    break
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
        of killing the whole battery with an unhandled SerialException.
        """
        try:
            return self._exchange_once(msg_type, data, raw, settle)
        except serial.SerialException:
            self.reopen()  # nucula CDC readiness quirk — one clean cycle
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

def conformance_battery(readers: dict[str, Reader]) -> bool:
    """Same commands through every reader; compare card-state semantics.

    Card-dependent cases (IccPowerOn, XfrBlock) adapt to each reader's
    detected ICC state: the bench card lives on the m5stick coil (issue
    #89 run, 2026-10-09: PowerOn legitimately SUCCEEDS there — activation
    retries landed with #88), so a hardcoded no-card expectation fails
    against correct firmware. Those cases are verified per-reader only
    and marked "n/a (card-relative)" — their outcomes depend on physical
    coupling, not on protocol divergence between readers.
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
        ("GetSlotStatus", MSG_GET_SLOT_STATUS, b"",
         lambda r, i: r and r[0][0] == 0x81 and (r[0][2] & 0x03) in (0x00, 0x01)),
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

    print(f"\nICC state: {icc} (0=present+active 1=present+inactive 2=no ICC)")
    print(f"{'case':28s} {'nucula':>30s}   {'m5stick':>30s}   verdict")
    for name, per in results.items():
        cells = []
        agree = True
        for rn in READERS:
            p, d = per[rn]
            cells.append(f"{d[:28]:>28s} {'PASS' if p else 'FAIL'}")
        # structural agreement: same response type AND same command-status
        # class (bits 6-7). Card-dependent cases are per-reader relative
        # (activation outcome depends on physical coupling — the bench card
        # lives on the m5stick coil) and never participate in the
        # reader-vs-reader comparison.
        sigs = []
        for rn in READERS:
            _p, d = per[rn]
            if d == "no response":
                sigs.append("none")
            else:
                t, st = d.split()[0], d.split()[1]
                sigs.append(f"{t}|{(int(st.split('=')[1], 16) >> 6) & 3}")
        agree = len(set(sigs)) == 1
        if name in ("IccPowerOn", "XfrBlock"):
            verdict = "n/a (card-relative)"
        else:
            verdict = "AGREE" if agree else "DIVERGE"
        print(f"{name:28s} {cells[0]:>34s}   {cells[1]:>34s}   {verdict}")
        if verdict == "DIVERGE":
            ok = False
    return ok


# ------------------------------------------------------------------- fuzz

def fuzz_battery(readers: dict[str, Reader], rounds: int) -> bool:
    """Malformed frames must never wedge the reader: every case ends with a
    valid GetSlotStatus answered correctly."""
    ok = True
    rng = random.Random(20261009)
    # Wire-silence check (issues #131/#133): no frames sent for 2 s —
    # the wires must be SILENT (clean-wire by construction: ceiling
    # before the driver claim; any bytes here are a regression or a
    # reintroduced unsolicited log line).
    print("\nwire silence (2 s quiet window, no frames sent):")
    for rname, r in readers.items():
        quiet = r.quiet_bytes(2.0)
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
        for name, raw in cases:
            r.seq = (r.seq % 255) + 1
            try:
                r.s.reset_input_buffer()
                r.s.write(raw)
            except serial.SerialException:
                # Issue #114 (Codex on #96): a dead port is a bench
                # fault — it must FAIL, never silently skip the case.
                if not r.reopen():
                    raise AssertionError(
                        f"{rname}: port died during fuzz case {name!r} "
                        "and reopen failed — bench hardware fault"
                    ) from None
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
                wedges[rname] += 1
                ok = False
                latency = recovery_latency(r, raw)
                print(f"  WEDGE {rname} after {name}: no valid GetSlotStatus in 3 probes"
                      + (f"; recovered after {latency:.2f}s" if latency is not None
                         else "; NO RECOVERY within 5s"))
        print(f"  {rname}: {len(cases)} fuzz cases, {wedges[rname]} wedges")
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
    ok = True
    for i in range(count):
        pcscd_start()
        out = subprocess.run(
            ["python3", "-c",
             "from smartcard.System import readers as R; "
             "print(any('Nucula' in str(r) for r in R()), "
             "any('GemPCTwin' in str(r) for r in R()))"],
            capture_output=True, text=True, timeout=30)
        out_line = out.stdout.strip()
        nuc, gem = out_line == "True True", out_line == "True True"
        if out_line not in ("True True", "False False"):
            nuc, gem = out_line.split() if " " in out_line else (False, False)
        if not (nuc and gem):
            ok = False
            print(f"  restart {i}: nucula={nuc} m5stick={gem} — FAIL")
            # the known CDC-state remedy: one clean open/close cycle
            try:
                s = serial.Serial(resolve("nucula"), 115200, timeout=0.5)
                s.close()
            except Exception:
                pass
        pcscd_stop()
    pcscd_start()
    print(f"  {'all restarts clean' if ok else 'some restarts needed the CDC remedy'}")
    return ok


def slot_status_soak(readers: dict[str, Reader], count: int) -> bool:
    print(f"\nGetSlotStatus soak ({count}x per reader):")
    ok = True
    for rname, r in readers.items():
        errs = 0
        first_icc = None
        t0 = time.time()
        for i in range(count):
            resp = r.exchange(MSG_GET_SLOT_STATUS, settle=0.01)
            ok = bool(resp and resp[0][0] == 0x81)
            if ok:
                icc = resp[0][2] & 7
                if first_icc is None:
                    first_icc = icc
                ok = icc == first_icc
            if not ok:
                errs += 1
        dt = time.time() - t0
        rate = count / dt if dt else 0
        print(f"  {rname}: {count - errs}/{count} clean ({rate:.0f}/s)")
        if errs:
            ok = False
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true", help="shrink fuzz/soak rounds")
    args = ap.parse_args()

    # Acquire the bench places BEFORE stopping pcscd or opening the
    # DUT ports (Codex on #107): the battery owns the hardware for the
    # whole run and must not race sibling sessions.
    places = [LABGRID_PLACES[name] for name in READERS]
    acquired = labgrid_acquire(places)
    if len(acquired) != len(places):
        labgrid_release(acquired)
        sys.exit(2)
    try:
        pcscd_stop()
        readers: dict[str, Reader] = {}
        try:
            for name in READERS:
                readers[name] = Reader(name, resolve(name))
            ok1 = conformance_battery(readers)
            ok2 = fuzz_battery(readers, 20 if args.quick else 80)
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
            ok4 = slot_status_soak(readers, 100 if args.quick else 1000)
        finally:
            for r in readers.values():
                r.close()
            pcscd_start()
    finally:
        labgrid_release(acquired)

    verdict = all([ok1, ok2, ok3, ok4])
    print(f"\nBATTERY VERDICT: {'PASS' if verdict else 'FAIL'}")
    sys.exit(0 if verdict else 1)


if __name__ == "__main__":
    main()
