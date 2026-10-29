#!/usr/bin/env python3
"""gp_probe — read-only GlobalPlatform identification probe (dry-run by default).

GP conformance lane (#139). Implements ONLY the zero-risk read set from
the card-handling doctrine (AGENTS.md) against a pcscd-visible reader:

  * SELECT ISD by full AID (A000000151000000 — bench-proven on the
    J3R180/P71 fleet, tests/hardware/nfc/cardlib.py:26-31)
  * GET DATA CPLC (80 CA 9F 7F 00 — the case-2 short-Le form proven on
    these cards, j3r180_gpg_lifecycle.py:117-121; the same shape
    GlobalPlatformPro sends, GPData.java:366-421)
  * OPTIONAL single-shot keyless GET STATUS (80 F2 80 02 02 4F 00 00 —
    GPPro's P1=0x80/P2=0x02 ISD query, GPSession.java:1043-1048) behind
    --allow-get-status: on JCOP-class cards GET STATUS rides the secure
    channel in practice (gp forces auth for -l, GPTool.java:1272-1281),
    so expect a 69xx refusal — one attempt, never retried.

SAFETY MODEL (regression-locked by tests/tools/test_gp_probe.py):

  1. DRY-RUN IS THE DEFAULT. Without --execute AND --reader the tool
     never opens hardware; pyscard is imported only inside the execute
     path (python-collect minimal-env rule).
  2. FAIL-CLOSED APDU ALLOWLIST. Every APDU passes guard_apdu(); the
     SC-class / state-changing GP commands (INITIALIZE UPDATE 0x50,
     EXTERNAL AUTHENTICATE 0x82, PUT KEY 0xD8, INSTALL 0xE6, LOAD 0xE8,
     STORE DATA 0xE2) and the doctrine-SC-class GET DATA 00E0 are
     REJECTED BY CONSTRUCTION — no code path builds them.
  3. SINGLE-SHOT. Each probe is transmitted once; a non-9000 answer is
     reported, never retried. Looping SC-class commands is the #144
     lockout mechanism — this tool cannot do it.

Offline decoders (no hardware): --parse-cplc (BER-TLV 9F7F unwrap +
field map per the in-repo GP CPLC lock, j3r180_gpg_lifecycle.py:149-155)
and --parse-init-update (INITIALIZE UPDATE response field map per
GlobalPlatformPro GPSession.java:363-445 — decode a captured hex string;
this tool never SENDS one).

--json-out writes a ccid_capture_diff.py (#143)-compatible capture for
the stage-2 cross-reader diff (docs/GP-CONFORMANCE-LANE.md §3.2).

Exit codes: 0 = ok (dry-run, decode, or all probes answered 9000);
1 = hardware/verdict failure; 2 = usage/parse error.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

# --- read-only APDU set (single source of truth for the builders) ---

ISD_AID = bytes.fromhex("A000000151000000")

# SELECT by full AID, no Le — the exact bench-proven form
# (cardlib.select_aid; j3r180 lifecycle "00A4040008A000000151000000").
APDU_SELECT_ISD = bytes([0x00, 0xA4, 0x04, 0x00, len(ISD_AID)]) + ISD_AID

# GET DATA CPLC, case-2 short Le (Le=00 -> 256) — GP CLA first; the
# 00-CLA fallback is GPPro's move (GPData.java:420-422) but a SECOND
# transmission is a retry: we send the GP form once and report.
APDU_GET_DATA_CPLC = bytes.fromhex("80CA9F7F00")

# GET STATUS ISD with TLV tags — GPPro's first GET STATUS
# (CLA_GP, INS_GET_STATUS, 0x80, 0x02, 4F00, Le=256).
APDU_GET_STATUS_ISD = bytes.fromhex("80F28002024F0000")

PROBES: list[dict] = [
    {
        "name": "SELECT_ISD",
        "apdu": APDU_SELECT_ISD,
        "rationale": "ISD exposure by full AID — pure read",
        "needs_flag": None,
    },
    {
        "name": "GET_DATA_CPLC",
        "apdu": APDU_GET_DATA_CPLC,
        "rationale": "keyless card identity — pure read",
        "needs_flag": None,
    },
    {
        "name": "GET_STATUS_ISD",
        "apdu": APDU_GET_STATUS_ISD,
        "rationale": "keyless registry query — usually SC-gated on "
                     "JCOP; single shot, refusal-tolerant",
        "needs_flag": "allow_get_status",
    },
]

# Doctrine SC-class / state-changing commands — REJECTED by guard_apdu.
# (GET DATA 00E0 Key Info Template is keyless in GPPro but doctrine
# classes it SC-class for this fleet: the locked card answers 69FF,
# issue #144.)
FORBIDDEN_INS = {
    0x50: "INITIALIZE UPDATE (SC-class, budget-bearing)",
    0x82: "EXTERNAL AUTHENTICATE (SC-class, budget-bearing)",
    0xD8: "PUT KEY (state-changing)",
    0xE6: "INSTALL (state-changing)",
    0xE8: "LOAD (state-changing)",
    0xE2: "STORE DATA (state-changing)",
}
ALLOWED_GET_DATA_TAGS = {(0x9F, 0x7F): "CPLC"}


def guard_apdu(apdu: bytes, *, allow_get_status: bool = False) -> tuple[bool, str]:
    """Fail-closed allowlist for every APDU this tool may transmit.

    Returns (allowed, reason). Anything not explicitly whitelisted is
    rejected — a future edit cannot widen the set by forgetting a check.
    """
    if len(apdu) < 4:
        return False, "too short for an APDU header"
    cla, ins, p1, p2 = apdu[0], apdu[1], apdu[2], apdu[3]
    if ins in FORBIDDEN_INS:
        return False, f"{FORBIDDEN_INS[ins]} rejected (card-handling doctrine)"
    if cla == 0x00 and ins == 0xA4:
        if p1 == 0x04 and p2 == 0x00 and apdu[5:] == ISD_AID and apdu[4] == len(ISD_AID):
            return True, "SELECT ISD by full AID (whitelisted read)"
        return False, "SELECT allowed only for the full ISD AID"
    if ins == 0xCA:
        if (p1, p2) in ALLOWED_GET_DATA_TAGS and cla in (0x80, 0x00):
            return True, f"GET DATA {ALLOWED_GET_DATA_TAGS[(p1, p2)]} (whitelisted read)"
        return False, f"GET DATA {p1:02X}{p2:02X} not in the keyless allowlist"
    if cla == 0x80 and ins == 0xF2:
        if allow_get_status:
            return True, "GET STATUS (opt-in single-shot keyless query)"
        return False, "GET STATUS needs --allow-get-status (usually SC-gated)"
    return False, f"not whitelisted: CLA={cla:02X} INS={ins:02X}"


# --- CPLC decode (offline) — layout locked in-repo ---
# GP Card Spec v2.3 §11.9 / Table 11-86 as locked at
# j3r180_gpg_lifecycle.py:149-155: ICS fabricator(2) ICS type(2) OS
# id(2) OS date(2) OS level(2) ICF fabricator(1) ICF type(1) ICF
# serial(4) ICF batch(2) ... The identity pin is bytes 0..16 (>= 32
# hex chars); serial/batch are the bench registry key
# (docs/HIL-VERIFICATION-PROTOCOL.md §1.1).
CPLC_IDENTITY_BYTES = 16


def unwrap_cplc_tlv(raw: bytes) -> bytes | None:
    """9F7F BER-TLV unwrap; None on any unexpected shape (fail closed)."""
    if len(raw) >= 2 and raw[0] == 0x9F and raw[1] == 0x7F:
        body = raw[2:]
        if not body:
            return None
        ln = body[0]
        if len(body) - 1 < ln:
            return None
        return body[1:1 + ln]
    return None


def parse_cplc(value_hex: str) -> dict:
    """Decode a CPLC VALUE (the 9F7F payload hex) into named fields."""
    try:
        value = bytes.fromhex(value_hex.strip())
    except ValueError as exc:
        raise ValueError(f"not hex: {exc}") from exc
    if len(value) < CPLC_IDENTITY_BYTES + 2:
        raise ValueError(
            f"CPLC value too short ({len(value)}B) — need >= "
            f"{CPLC_IDENTITY_BYTES + 2}B for serial+batch"
        )
    fields = {
        "ics_fabricator": value[0:2].hex(),
        "ics_type": value[2:4].hex(),
        "os_id": value[4:6].hex(),
        "os_date": value[6:8].hex(),
        "os_level": value[8:10].hex(),
        "icf_fabricator": value[10:11].hex(),
        "icf_type": value[11:12].hex(),
        "icf_serial": value[12:16].hex(),
        "icf_batch": value[16:18].hex(),
        "identity_hex": value[:CPLC_IDENTITY_BYTES].hex().upper(),
        "registry_key": f"{value[12:16].hex().upper()}/{value[16:18].hex().upper()}",
        "trailing": value[18:].hex(),
    }
    return fields


# --- INITIALIZE UPDATE response decode (offline only) ---
# Field map per GlobalPlatformPro GPSession.java:363-445 (branch next,
# commit dd1a4c6, 2026-09-29): SCP01/SCP02 = 28B, SCP03 S8 = 29(32)B,
# SCP03 S16 = 45(48)B; key info = kvn | scp [| i for SCP03]. The bench
# "#127 i-params fingerprint" is response bytes 12..15 — pinned here so
# future captures name fields exactly (docs/GP-CONFORMANCE-LANE.md §1.1).

def parse_init_update(resp_hex: str) -> dict:
    """Decode a CAPTURED INITIALIZE UPDATE response hex (never sends one)."""
    try:
        resp = bytes.fromhex(resp_hex.strip())
    except ValueError as exc:
        raise ValueError(f"not hex: {exc}") from exc
    if len(resp) < 28:
        raise ValueError(f"too short ({len(resp)}B) — min response is 28B")
    kdd = resp[0:10]
    kvn = resp[10]
    scp = resp[11]
    out: dict = {
        "kdd": kdd.hex().upper(),
        "key_version": f"{kvn:02X}",
        "scp": f"{scp:02X}",
        "length": len(resp),
        "bench_i_fingerprint_127": resp[12:16].hex().upper(),
    }
    if scp in (0x01, 0x02):
        if len(resp) != 28:
            raise ValueError(
                f"SCP{scp:02X} response must be 28B, got {len(resp)}B — "
                "a length deviation is itself a transport finding"
            )
        if scp == 0x02:
            out["sequence_counter"] = resp[12:14].hex().upper()
            out["card_challenge"] = resp[14:20].hex().upper()
            out["card_cryptogram"] = resp[20:28].hex().upper()
        else:
            out["card_challenge"] = resp[12:20].hex().upper()
            out["card_cryptogram"] = resp[20:28].hex().upper()
    elif scp == 0x03:
        i = resp[12]
        s16 = bool(i & 0x01)
        pseudo = bool(i & 0x10)
        expected = 29 + (3 if pseudo else 0) + (16 if s16 else 0)
        if len(resp) not in (expected, expected + 3):
            raise ValueError(
                f"SCP03 i={i:02X} implies {expected}B (+3 optional tail), "
                f"got {len(resp)}B"
            )
        ch_len = 16 if s16 else 8
        out["i"] = f"{i:02X}"
        out["s16"] = s16
        out["pseudorandom_challenge"] = pseudo
        out["card_challenge"] = resp[13:13 + ch_len].hex().upper()
        out["card_cryptogram"] = resp[13 + ch_len:13 + 2 * ch_len].hex().upper()
        if pseudo:
            seq_at = 13 + 2 * ch_len
            out["sequence_counter"] = resp[seq_at:seq_at + 3].hex().upper()
    else:
        raise ValueError(f"unsupported SCP byte 11 = {scp:02X}")
    return out


# --- hardware path (lazy pyscard, single-shot transmissions) ---

def _connect(reader_substr: str):
    from smartcard.Exceptions import CardConnectionException, NoCardException
    from smartcard.System import readers

    matches = [r for r in readers() if reader_substr in str(r)]
    if not matches:
        print(f"ERROR: no pcscd reader matches {reader_substr!r}", file=sys.stderr)
        return None
    if len(matches) > 1:
        print(f"ERROR: ambiguous reader {reader_substr!r}: {matches}",
              file=sys.stderr)
        return None
    conn = matches[0].createConnection()
    for _ in range(3):  # connection-level retries only (power-up class)
        try:
            conn.connect()
            return conn, bytes(conn.getATR()).hex().upper()
        except (NoCardException, CardConnectionException):
            time.sleep(1.5)
    print("ERROR: card connection failed (3 attempts)", file=sys.stderr)
    return None


def execute_probes(reader_substr: str, *, allow_get_status: bool,
                   json_out: Path | None, atr_out: list) -> int:
    got = _connect(reader_substr)
    if got is None:
        return 1
    conn, atr = got
    atr_out.append(atr)
    exchanges: list[dict] = []
    failures = 0
    try:
        for probe in PROBES:
            if probe["needs_flag"] and not allow_get_status:
                continue
            apdu = probe["apdu"]
            ok, reason = guard_apdu(apdu, allow_get_status=allow_get_status)
            if not ok:  # construction bug — never transmit
                print(f"GUARD REFUSED {probe['name']}: {reason}", file=sys.stderr)
                failures += 1
                continue
            data, sw1, sw2 = conn.transmit(list(apdu))  # single shot
            resp = bytes(data)
            sw = f"{sw1:02X}{sw2:02X}"
            exchanges.append({
                "name": probe["name"],
                "command": apdu.hex(),
                "response": (resp.hex() + sw).lower(),
            })
            print(f"{probe['name']}: SW={sw} ({len(resp)}B data)")
            if sw != "9000":
                print("  note: non-9000 answer reported, NOT retried "
                      "(single-shot doctrine)")
                if probe["name"] != "GET_STATUS_ISD":
                    failures += 1
            if probe["name"] == "GET_DATA_CPLC" and sw == "9000":
                value = unwrap_cplc_tlv(resp)
                if value is None:
                    print("  CPLC response is not a 9F7F BER-TLV — unreadable")
                    failures += 1
                else:
                    # Codex P2 on #500: a structurally valid TLV with a
                    # short body must FAIL the verdict, never raise past
                    # the JSON-writing block — the malformed capture is
                    # exactly what a corruption investigation needs kept.
                    try:
                        fields = parse_cplc(value.hex())
                    except ValueError as e:
                        print(f"  CPLC decode FAILED ({e}) — raw kept in capture")
                        failures += 1
                    else:
                        print(f"  CPLC registry key: {fields['registry_key']} "
                              f"(identity {fields['identity_hex'][:16]}…)")
                        atr_out.append(fields)
    finally:
        try:
            conn.disconnect()
        except Exception:
            pass
    if json_out is not None:
        capture = {
            "reader": reader_substr,
            "card": atr,
            "firmware_rev": "n/a",
            "captured_at": datetime.now(timezone.utc).isoformat(),
            "exchanges": exchanges,
        }
        json_out.write_text(json.dumps(capture, indent=2) + "\n")
        print(f"capture written: {json_out}")
    return 1 if failures else 0


# --- CLI ---

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="gp_probe",
        description="Read-only GP identification probe (dry-run by default)",
    )
    parser.add_argument("--execute", action="store_true",
                        help="talk to hardware (requires --reader)")
    parser.add_argument("--reader", metavar="SUBSTR",
                        help="pcscd reader name substring (stable identity)")
    parser.add_argument("--allow-get-status", action="store_true",
                        help="also send ONE keyless GET STATUS (usually "
                             "SC-gated on JCOP; refusal tolerated)")
    parser.add_argument("--json-out", type=Path, metavar="PATH",
                        help="write a ccid_capture_diff.py-compatible capture")
    parser.add_argument("--parse-cplc", metavar="HEX",
                        help="offline: decode a CPLC 9F7F VALUE hex")
    parser.add_argument("--verbose", action="store_true",
                        help="per-exchange hex dumps on stdout (quiet by default)")
    parser.add_argument("--dry-run", action="store_true",
                        help="accepted for runbook compatibility; probe is "
                             "read-only and hardware access requires --execute "
                             "(dry-run IS the default behavior)")
    parser.add_argument("--parse-init-update", metavar="HEX",
                        help="offline: decode a CAPTURED INITIALIZE UPDATE "
                             "response hex (never sends one)")
    args = parser.parse_args(argv)

    if args.parse_cplc:
        try:
            for k, v in parse_cplc(args.parse_cplc).items():
                print(f"{k:24s} {v}")
        except ValueError as exc:
            print(f"ERROR: {exc}", file=sys.stderr)
            return 2
        return 0

    if args.parse_init_update:
        try:
            for k, v in parse_init_update(args.parse_init_update).items():
                print(f"{k:26s} {v}")
        except ValueError as exc:
            print(f"ERROR: {exc}", file=sys.stderr)
            return 2
        return 0

    if args.execute:
        if not args.reader:
            print("ERROR: --execute requires --reader", file=sys.stderr)
            return 2
        atr: list = []
        return execute_probes(args.reader, allow_get_status=args.allow_get_status,
                              json_out=args.json_out, atr_out=atr)

    # DRY RUN (default): the APDU table + guard verdicts, no hardware.
    print("gp_probe DRY RUN — no hardware is opened (pass --execute --reader …)")
    for probe in PROBES:
        if probe["needs_flag"]:
            print(f"{probe['name']:16s} SKIPPED (needs --{probe['needs_flag'].replace('_', '-')})")
            continue
        ok, reason = guard_apdu(probe["apdu"])
        print(f"{probe['name']:16s} {probe['apdu'].hex().upper():24s} "
              f"{'OK' if ok else 'REFUSED':8s} {reason}")
    print("\nSC-class commands (INIT UPDATE / EXT AUTH / PUT KEY / INSTALL /"
          " LOAD / STORE DATA) are not implemented and are rejected by the"
          " guard — card-handling doctrine (AGENTS.md, issues #127/#144).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
