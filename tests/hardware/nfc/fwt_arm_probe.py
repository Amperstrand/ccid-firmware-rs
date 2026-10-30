#!/usr/bin/env python3
"""FWT-arming probe for the m5stick MFRC522 CCID reader (bolty #180 evidence).

Reads the reader's ARMED frame-wait timer through Escape 0xD3 (issue
#120's probe payload: [total_us, reads, configured_fwt_ms] LE) and
compares it against the FWI a card's ATS/ATR advertises — the on-wire
fingerprint of the #180 fix class:

  PRE-FIX  fw (fixed 0xA9 prescaler): configured_fwt_ms caps at 1638 —
           an FWI 13/14 card (FWT 2483/4957 ms) times out early.
  POST-FIX fw (ccid PR #464): configured_fwt_ms == the FWI-derived FWT
           (2483 / 4957 for FWI 13/14); values <= 1638 ms are
           byte-identical on both fw generations (no fingerprint).

Two transports:
  pcscd (default; safe while the reader serves pcscd): SCardControl
      FEATURE_CCID_ESC (CTL_CODE(1)); requires ifdDriverOptions=0x0001
      in ifd-ccid.bundle (bench standard).
  --via-serial (port must be free — pcscd stopped; turnaround_probe.py
      rules): raw GemPC framing on the by-id port.

Card legs are gated on a card being on the coil: with a card (pcscd
path) the probe connects, parses TB(1) BWI from the T=1 ATR, then reads
0xD3 and checks armed-vs-advertised.  Without a card it still proves
the plumbing and reports the idle armed FWT.

Usage:
  python3 fwt_arm_probe.py                  # pcscd path, baseline + card if present
  python3 fwt_arm_probe.py --via-serial     # raw port (pcscd stopped)
  python3 fwt_arm_probe.py --expect-fwi 14  # assert the card leg (bench gate)
"""
import argparse
import struct
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # tests/hardware

from nfc.conformance_battery import MSG_ESCAPE, frame, parse_frames  # noqa: E402

READER_NEEDLE = "M5stick"  # pcscd reader name substring (stable identity)

# CCID Rev 1.1: 10-byte header = type(1) dwLength(4) bSlot(1) bSeq(1)
# bRFU(2) bSpecific(1); payload follows.
CCID_PC_TO_RDR_ESCAPE = 0x6B
CCID_HEADER_SIZE = 10


def fwt_ms_for_fwi(fwi: int) -> float:
    """ISO 14443-3 §7.2 FWT = 256 x 16 / fc x 2^FWI, fc = 13.56 MHz (ms).
    FWI 13 -> ~2475 ms, FWI 14 -> ~4949 ms (bolty #180's cases, quoted
    there as 2483/4957 per its own derivation; the probe compares with
    1% tolerance and against the 1638 ms clamp fingerprint)."""
    return 256 * 16 / 13.56e6 * (2 ** fwi) * 1000.0


def fwi_from_atr(atr: bytes):
    """BWI from a T=1 ATR's TB(1) (ISO 7816-3: BWI = TB(1) high nibble).
    None when TB(1) is absent (BWI defaults to 0) or unparseable. NOTE:
    for NFC cards the FWI that matters lives in the ATS TB(1), which the
    ATR synthesis does NOT carry — the card leg should be driven by
    --expect-fwi (known card class); this parser only catches classic
    contact ATRs that do encode TB(1)."""
    if len(atr) < 2 or atr[0] not in (0x3B, 0x3F):
        return None
    presence = atr[1] & 0xF0  # TA1/TB1/TC1/TD1 presence bits (T0 upper)
    idx = 2
    tb1 = None
    group = 1
    while presence and idx < len(atr) and group <= 4:
        if presence & 0x10:  # TA(n)
            idx += 1
        if presence & 0x20:  # TB(n)
            if group == 1:
                tb1 = atr[idx]
            idx += 1
        if presence & 0x40:  # TC(n)
            idx += 1
        if presence & 0x80 and idx < len(atr):  # TD(n): next group
            td = atr[idx]
            idx += 1
            presence = td & 0xF0
            group += 1
        else:
            break
    return (tb1 >> 4) if tb1 is not None else None


def _feature_ioctl(feat_tlv: bytes, want_tag: int):
    """Walk the Part10 CM_IOCTL_GET_FEATURE_REQUEST TLV
    (tag u8, len u8 == 4, value u32 ioctl LE) — return the ioctl for
    want_tag, None when absent."""
    i = 0
    while i + 2 <= len(feat_tlv):
        tag, ln = feat_tlv[i], feat_tlv[i + 1]
        if i + 2 + ln > len(feat_tlv) or ln != 4:
            return None
        if tag == want_tag:
            return struct.unpack("<I", feat_tlv[i + 2:i + 6])[0]
        i += 2 + ln
    return None


def parse_d3_payload(abdata: bytes):
    """Escape abData: [total_us u32][reads u32][fwt_ms u32] LE. pcscd
    control returns the payload directly (exactly 12 bytes); the serial
    path carries the RDR header in front (type byte 0x80..0x83, >= 22
    bytes total). Anything else does not parse."""
    if len(abdata) == 12:
        return struct.unpack("<III", abdata)
    if len(abdata) >= CCID_HEADER_SIZE + 12 and 0x80 <= abdata[0] <= 0x83:
        return struct.unpack("<III",
                             abdata[CCID_HEADER_SIZE:CCID_HEADER_SIZE + 12])
    return None


def build_escape_msg(seq: int, data: bytes) -> bytes:
    """Full 10-byte-header PC_to_RDR_Escape message for SCardControl."""
    return (struct.pack("<BI BB H B", CCID_PC_TO_RDR_ESCAPE, len(data),
                        0, seq, 0, 0)
            + data)


def _verdict(fwt_ms, expect_fwi):
    want = fwt_ms_for_fwi(expect_fwi)
    print(f"expect FWI {expect_fwi}: advertised FWT = {want:.0f} ms "
          f"(tolerance 1%)")
    if fwt_ms is None or fwt_ms < want * 0.99:
        print(f"VERDICT: PRE-FIX class — armed {fwt_ms} ms < {want:.0f} ms "
              "(the 1638 ms clamp fingerprint of bolty #180; fw predates "
              "ccid PR #464)")
        return 2
    print(f"VERDICT: FIXED — armed {fwt_ms} ms covers the advertised FWT "
          "(ccid PR #464 arithmetic)")
    return 0


def via_pcscd(expect_fwi=None):
    """Escape path (track_d.py feature pattern). Card leg: high-level
    CardConnection (connect arms the card FWT). Card-less baseline: raw
    DIRECT connection + Part10 CM_IOCTL_GET_FEATURE_REQUEST TLV —
    control works on a card-less reader only through SHARE_DIRECT."""
    from smartcard.System import readers

    matches = [r for r in readers() if READER_NEEDLE in str(r)]
    if not matches:
        print(f"SKIP: no pcscd reader matching {READER_NEEDLE!r}")
        return None
    reader = matches[0]
    print(f"reader: {reader}")

    atr = None
    fwt_ms = None
    conn = reader.createConnection()
    try:
        conn.connect()  # card on coil: activation arms the card FWT
        atr = bytes(conn.getATR())
        from smartcard.pcsc.PCSCPart10 import (FEATURE_CCID_ESC_COMMAND,
                                               getFeatureRequest)
        features = dict(getFeatureRequest(conn))
        ioctl = features.get(FEATURE_CCID_ESC_COMMAND)
        if ioctl is None:
            print("SKIP: FEATURE_CCID_ESC_COMMAND not advertised")
        else:
            resp = bytes(conn.control(ioctl, list(b"\xD3\x20")))
            words = parse_d3_payload(resp)
            if words:
                _, _, fwt_ms = words
    except Exception as e:  # noqa: BLE001 — no card is a normal state
        print(f"no card session ({type(e).__name__}) — card-less baseline "
              "via DIRECT control")
    finally:
        try:
            conn.disconnect()
        except Exception:  # noqa: BLE001
            pass

    if atr is not None:
        bwi = fwi_from_atr(atr)
        print(f"card ATR: {atr.hex()} (TB1 BWI -> "
              f"{f'FWI {bwi}' if bwi is not None else 'TB(1) absent'})")
        if bwi is not None and expect_fwi is None:
            expect_fwi = bwi
    if fwt_ms is not None:
        total_us, reads, fwt_ms = words
        print(f"escape 0xD3: {reads} ComIrqReg reads in {total_us} us "
              f"({total_us / max(reads, 1):.0f} us/read); "
              f"armed FWT = {fwt_ms} ms")
        if expect_fwi is not None:
            return _verdict(fwt_ms, expect_fwi), fwt_ms
        return fwt_ms

    # card-less: raw DIRECT handle + feature TLV walk
    from smartcard.scard import (SCARD_LEAVE_CARD, SCARD_SCOPE_USER,
                                 SCARD_SHARE_DIRECT, SCardConnect,
                                 SCardControl, SCardDisconnect,
                                 SCardEstablishContext, SCARD_CTL_CODE)
    hresult, hctx = SCardEstablishContext(SCARD_SCOPE_USER)
    if hresult != 0:
        print(f"FAIL: establish rc={hresult:#x}")
        return None
    hresult, hcard, _ = SCardConnect(hctx, str(reader), SCARD_SHARE_DIRECT, 0)
    if hresult != 0:
        print(f"FAIL: DIRECT connect rc={hresult:#x}")
        return None
    try:
        rc, feat = SCardControl(hcard, SCARD_CTL_CODE(3400), [])
        if rc != 0:
            print(f"FAIL: feature request rc={rc:#x}")
            return None
        ioctl = _feature_ioctl(bytes(feat), 19)
        if ioctl is None:
            print("SKIP: FEATURE_CCID_ESC_COMMAND not advertised through "
                  "the serial stack (libccid ifdDriverOptions=0x0001 "
                  "gates ESC)")
            return None
        rc, resp = SCardControl(hcard, ioctl, [0xD3, 32])
        if rc != 0:
            print(f"FAIL: 0xD3 control rc={rc:#x}")
            return None
        words = parse_d3_payload(bytes(resp))
        if words is None:
            print(f"FAIL: short 0xD3 payload {bytes(resp).hex()}")
            return None
        total_us, reads, fwt_ms = words
        print(f"escape 0xD3 (card-less): {reads} ComIrqReg reads in "
              f"{total_us} us ({total_us / max(reads, 1):.0f} us/read); "
              f"idle armed FWT = {fwt_ms} ms")
        return fwt_ms
    finally:
        SCardDisconnect(hcard, SCARD_LEAVE_CARD)


def via_serial(port=None):
    import serial

    from serial_ports import resolve

    port = port or next(iter(resolve("m5stick")), None)
    if not port:
        print("SKIP: no m5stick serial port")
        return None
    s = serial.Serial(port, 115200, bytesize=8, parity="N", stopbits=2,
                      timeout=0.5)
    s.setDTR(False)
    s.setRTS(False)
    try:
        seq = 1
        s.reset_input_buffer()
        s.write(frame(MSG_ESCAPE, seq, bytes([0xD3, 32])))
        buf = b""
        t0 = time.time()
        while time.time() - t0 < 2.0:
            n = s.in_waiting
            if n:
                buf += s.read(n)
                for f in parse_frames(buf):
                    if f[0] >= 0x80 and f[1] == seq:
                        payload = f[2]
                        if len(payload) < CCID_HEADER_SIZE + 12:
                            break
                        total_us, reads, fwt_ms = parse_d3_payload(
                            payload[CCID_HEADER_SIZE:])
                        print(f"escape 0xD3: {reads} reads in "
                              f"{total_us} us; armed FWT = {fwt_ms} ms")
                        return fwt_ms
            else:
                time.sleep(0.01)
        print(f"FAIL: no 0xD3 response ({buf.hex()[:80]})")
        return None
    finally:
        s.close()


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--via-serial", action="store_true")
    ap.add_argument("--port", default=None, help="serial path (serial mode)")
    ap.add_argument("--expect-fwi", type=int, default=None,
                    help="assert the armed FWT covers this FWI (bench gate; "
                         "auto-driven from the card ATR when present)")
    args = ap.parse_args(argv)

    out = via_serial(args.port) if args.via_serial else via_pcscd(args.expect_fwi)
    if isinstance(out, tuple):  # verdict path already ran
        return out[0]
    fwt_ms = out
    if fwt_ms is None:
        return 1  # FAIL/SKIP already printed
    print("no card-derived FWI and no --expect-fwi: baseline report only "
          "(plumbing proof / idle armed FWT)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
