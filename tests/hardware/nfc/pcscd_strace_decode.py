#!/usr/bin/env python3
"""pcscd_strace_decode — decode GemPC Twin wire traffic from a pcscd strace.

Companion to gempc_t1.py (see GEMPC_WIRE_HARNESS.md): capture what
libccidtwin ACTUALLY puts on the wire while pcscd drives the reader,
then decode every GemPC frame + T=1 block with LRC/EDC verification.
Replaying a captured sequence against the device is gempc_t1's job
(T1Session drives the same session shape live).

⚠⚠ -xx IS LOAD-BEARING — ALWAYS capture with hex escapes: ⚠⚠

    strace -f -e trace=read,write -xx -s 4096 -o TRACE.txt -- pcscd -f
    (attach to a running pcscd: same options + -p <pid>)

Without -xx, strace falls back to OCTAL (\\012) and LETTER (\\f, \\n,
\\t, \\r, \\v) escapes — octal/letter escapes silently corrupt
\\f/\\n/\\t bytes in any decoder that assumes \\xNN, and even when
decoded they invite ambiguity on a binary protocol wire. This tool
still understands the legacy forms (and warns loudly when it meets
them) — but the fix is to re-capture with -xx, not to trust the
decode. -s 4096 matters too: a truncated payload prints as a literal
"..." tail and the frame count will be wrong.

fd pinning BY DEVICE: pcscd talks to several readers through several
fds; the GemPC-talking fd is found by resolving /proc/<pid>/fd/<n>
symlinks of the pids seen in the trace (targets under /dev/tty* or
/dev/serial/*), optionally filtered by a --device substring (e.g.
--device Hades for the m5stick FTDI). When pcscd is already dead,
pass --fd N explicitly.

The RX stream is the REassembled read() stream in capture order —
one GemPC frame regularly arrives split across several read()s, and
the echo of TX precedes the response in it.

Usage:
  python3 pcscd_strace_decode.py TRACE.txt                 # both dirs
  python3 pcscd_strace_decode.py TRACE.txt --device Hades
  python3 pcscd_strace_decode.py TRACE.txt --fd 6 --direction tx
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path
from typing import NamedTuple

sys.path.insert(0, str(Path(__file__).resolve().parent))

from gempc_t1 import (  # noqa: E402
    CCID_TYPE_NAMES,
    MSG_XFR_BLOCK,
    RDR_DATA_BLOCK,
    parse_frames,
    parse_t1,
)

# strace line shapes (-f threads interleave, producing
# <unfinished ...> / <... resumed> pairs around the payload):
#   12345  write(6, "\x03\x06...", 13)           = 13
#   [pid 12345] write(6, "\x03\x06...", 13)     = 13
#   12345  read(6,  <unfinished ...>
#   12345  <... read resumed>"\x03\x06...", 13) = 13
_DATA_GROUP = r'"(?P<data>(?:[^"\\]|\\.)*)"'
_SYSCALL_RE = re.compile(
    r'(?:\[pid\s+(?P<pid>\d+)\]\s*)?(?:(?P<tid>\d+)\s+)?'
    r'(?P<op>read|write)\((?P<fd>\d+),\s*'
    + _DATA_GROUP +
    r',\s*(?P<count>\d+)\)\s*=\s*(?P<ret>-?\d+)')
_UNFINISHED_RE = re.compile(
    r'(?:\[pid\s+(?P<pid>\d+)\]\s*)?(?:(?P<tid>\d+)\s+)?'
    r'(?P<op>read|write)\((?P<fd>\d+)\s*,?\s*'
    r'(?:' + _DATA_GROUP + r')?(?:,\s*(?P<count>\d+))?'
    r'\s*<unfinished \.\.\.>')
_RESUMED_RE = re.compile(
    r'(?:\[pid\s+(?P<pid>\d+)\]\s*)?(?:(?P<tid>\d+)\s+)?'
    r'<\.\.\.\s*(?P<op>read|write)\s+resumed>'
    r'(?:,?\s*' + _DATA_GROUP + r')?(?:,\s*(?P<count>\d+))?'
    r'\)\s*=\s*(?P<ret>-?\d+)')

_LETTER_ESCAPES = {"a": 0x07, "b": 0x08, "f": 0x0C, "n": 0x0A,
                   "r": 0x0D, "t": 0x09, "v": 0x0B}
_HEXDIGITS = "0123456789abcdefABCDEF"


class Syscall(NamedTuple):
    pid: int | None
    op: str          # "read" | "write"
    fd: int
    data: bytes
    ret: int


def _device_matches(device: str, proc_link: str) -> bool:
    """Codex P2 on #347: /proc/<pid>/fd symlinks resolve to the real
    dentry (/dev/ttyUSB0), NOT the stable /dev/serial/by-id path a user
    passes — resolve the request the same way before comparing."""
    want = os.path.realpath(device)
    got = os.path.realpath(proc_link)
    if want == got:
        return True
    # explicit tty names and substrings still work as a fallback
    return device in proc_link or device in got


def unescape(s: str) -> bytes:
    """strace string-literal -> bytes.

    Handles \\xHH (-xx), \\\\ and \\" (always present), and the legacy
    letter (\\n \\t \\f ...) and octal (\\NNN) forms for captures made
    without -xx — legacy_escape_count() tells you when that happened.
    """
    out = bytearray()
    i = 0
    n = len(s)
    while i < n:
        c = s[i]
        if c != "\\":
            out.append(ord(c) & 0xFF)
            i += 1
            continue
        nxt = s[i + 1] if i + 1 < n else ""
        if nxt == "x":
            j = i + 2
            h = ""
            while j < n and len(h) < 2 and s[j] in _HEXDIGITS:
                h += s[j]
                j += 1
            if h:
                out.append(int(h, 16))
                i = j
                continue
            out.append(0x5C)  # lone \x -> literal backslash
            i += 1
            continue
        if nxt in _LETTER_ESCAPES:
            out.append(_LETTER_ESCAPES[nxt])
            i += 2
            continue
        if nxt in "01234567":
            j = i + 1
            o = ""
            while j < n and len(o) < 3 and s[j] in "01234567":
                o += s[j]
                j += 1
            out.append(int(o, 8) & 0xFF)
            i = j
            continue
        if nxt in ('"', "\\", "'"):
            out.append(ord(nxt))
            i += 2
            continue
        out.append(0x5C)
        i += 1
    return bytes(out)


def legacy_escape_count(text: str) -> int:
    """Non-\\xNN escapes present (0 for a proper -xx capture)."""
    n = 0
    i = 0
    while i < len(text):
        if text[i] == "\\" and i + 1 < len(text):
            nxt = text[i + 1]
            if nxt in ('"', "\\", "'"):
                i += 2
                continue
            if nxt == "x":
                i += 4
                continue
            if nxt in "01234567":
                j = i + 1
                while j < len(text) and j < i + 4 and text[j] in "01234567":
                    j += 1
                n += 1
                i = j
                continue
            if nxt in _LETTER_ESCAPES:
                n += 1
                i += 2
                continue
        i += 1
    return n


def _tid(m: re.Match) -> str:
    return m.group("pid") or m.group("tid") or "-"


def parse_strace(text: str) -> list[Syscall]:
    """All read/write syscalls with their data, in capture order.

    Handles -f thread interleaving (<unfinished ...>/<... resumed>):
    a read's payload arrives on the resumed line, a write's may arrive
    on either; the tid keys the pending map.
    """
    events: list[Syscall] = []
    pending: dict[str, tuple[str, int, str | None]] = {}
    for ln in text.splitlines():
        m = _SYSCALL_RE.search(ln)
        if m:
            ret = int(m.group("ret"))
            if ret >= 0:
                data = unescape(m.group("data"))[:ret]
                events.append(Syscall(
                    int(m.group("pid") or m.group("tid") or 0) or None,
                    m.group("op"), int(m.group("fd")), data, ret))
            continue
        m = _UNFINISHED_RE.search(ln)
        if m:
            raw = m.group("data")  # write payloads may already be visible
            pending[_tid(m)] = (m.group("op"), int(m.group("fd")), raw)
            continue
        m = _RESUMED_RE.search(ln)
        if m:
            state = pending.pop(_tid(m), None)
            if state is None:
                continue
            op, fd, pre_raw = state
            ret = int(m.group("ret"))
            raw = m.group("data") if m.group("data") is not None else pre_raw
            if raw is None or ret <= 0:
                continue  # EOF / no bytes
            events.append(Syscall(
                int(m.group("pid") or m.group("tid") or 0) or None,
                op, fd, unescape(raw)[:ret], ret))
    return events


def is_serial_device(link: str) -> bool:
    return "/dev/tty" in link or "/dev/serial" in link


def pin_fd(events: list[Syscall], device: str | None = None,
           fd_arg: int | None = None) -> tuple[int, str]:
    """Pick the GemPC-talking fd BY DEVICE (or --fd override).

    Returns (fd, provenance note). Raises SystemExit with a listing
    when /proc cannot disambiguate — never silently guesses.
    """
    if fd_arg is not None:
        return fd_arg, "--fd override (no /proc verification)"
    by_fd: dict[int, set[int]] = {}
    for e in events:
        by_fd.setdefault(e.fd, set()).add(e.pid or 0)
    candidates: dict[int, str] = {}
    for fd, pids in sorted(by_fd.items()):
        for pid in sorted(p for p in pids if p):
            try:
                link = os.readlink(f"/proc/{pid}/fd/{fd}")
            except OSError:
                continue
            if not is_serial_device(link):
                continue
            if device is not None and not _device_matches(device, link):
                continue
            candidates[fd] = link
            break
    if len(candidates) == 1:
        fd, link = next(iter(candidates.items()))
        return fd, f"/proc symlink -> {link}"
    listing = "\n".join(f"  fd {fd}: {link}" for fd, link in candidates.items())
    if not candidates:
        raise SystemExit(
            "no serial fd could be resolved via /proc (pcscd gone?). "
            "Re-run with --fd N (fds seen: "
            f"{sorted(by_fd)}) or capture while pcscd still lives.")
    raise SystemExit(
        f"multiple serial fds matched:\n{listing}\n"
        "Disambiguate with --device <substring> or --fd N.")


def decode_stream(stream: bytes, direction: str) -> list[str]:
    """Decode one reassembled direction into report lines."""
    frames = parse_frames(stream)
    covered = sum(13 + len(f.payload) for f in frames)
    lines = [f"{direction}: {len(stream)} bytes on the wire, "
             f"{len(frames)} LRC-valid frame(s), "
             f"{len(stream) - covered} noise/split byte(s)"]
    for i, f in enumerate(frames):
        name = CCID_TYPE_NAMES.get(f.msg_type, "?")
        lines.append(f"  [{i:3d}] type={f.msg_type:02X} {name:<14} "
                     f"seq={f.seq:<3d} bStatus={f.bstatus:02X} "
                     f"plen={len(f.payload)}")
        if f.msg_type in (MSG_XFR_BLOCK, RDR_DATA_BLOCK) and f.payload:
            tb = parse_t1(f.payload)
            if tb is not None:
                prev = tb.inf[:16].hex()
                if len(tb.inf) > 16:
                    prev += "..."
                lines.append(f"        T=1 {tb.describe()} INF={prev}")
            else:
                lines.append(f"        (not a T=1 block) "
                             f"payload={f.payload[:32].hex()}")
    return lines


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Decode GemPC Twin wire traffic from a pcscd strace "
                    "(capture with: strace -f -e trace=read,write -xx "
                    "-s 4096)")
    ap.add_argument("trace", help="strace output file")
    ap.add_argument("--device", default=None,
                    help="substring of the /dev fd symlink to pin "
                         "(e.g. Hades for the m5stick FTDI)")
    ap.add_argument("--fd", type=int, default=None,
                    help="pin the fd number explicitly (when pcscd is "
                         "gone and /proc cannot resolve it)")
    ap.add_argument("--direction", choices=("tx", "rx", "both"),
                    default="both",
                    help="tx = pcscd->reader writes, rx = reader->pcscd "
                         "reads (echo + responses); default both")
    args = ap.parse_args(argv)

    text = Path(args.trace).read_text(errors="replace")
    legacy = legacy_escape_count(text)
    if legacy:
        print("⚠ WARNING: "
              f"{legacy} octal/letter escape(s) in this capture — it was "
              "NOT taken with -xx. Octal/letter escapes silently corrupt "
              "\\f/\\n/\\t bytes; decode with care and RE-CAPURE with "
              "`strace -f -e trace=read,write -xx -s 4096`.")
    events = parse_strace(text)
    if not events:
        print("no read/write syscalls parsed from the trace")
        return 2
    fd, why = pin_fd(events, args.device, args.fd)
    writes = [e for e in events if e.fd == fd and e.op == "write"]
    reads = [e for e in events if e.fd == fd and e.op == "read"]
    print(f"pinned fd={fd} ({why}); {len(writes)} write(s), "
          f"{len(reads)} read(s)")

    tx = b"".join(e.data for e in writes)
    rx = b"".join(e.data for e in reads)
    n_frames = 0
    if args.direction in ("tx", "both"):
        lines = decode_stream(tx, "TX (pcscd -> reader)")
        n_frames += len(parse_frames(tx))
        print("\n".join(lines))
    if args.direction in ("rx", "both"):
        lines = decode_stream(rx, "RX (reader -> pcscd, echo first)")
        n_frames += len(parse_frames(rx))
        print("\n".join(lines))
    if n_frames == 0:
        print("no LRC-valid GemPC frames found — wrong fd, or the "
              "capture is truncated (check -s 4096)")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
