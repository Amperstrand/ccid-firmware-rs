#!/usr/bin/env python3
"""fixture_selfcheck.py — one-command NFC bench reality check (#529 lab).

Probes every NFC fixture claim and prints the drift between expected
and actual state. Generated status, never hand-written (the #183
doctrine applied to the NFC bench): coil occupants are matched by ATR
signature, not by trust.

Checks:
  1. m5stick lane pcscd (ai-legion): socket answers, reader listed,
     coil occupant ATR (via SCardConnect — works for HCE-class cards;
     known-bug classes are annotated, not failed).
  2. ai-legion-small system pcscd: ACR1252 PICC occupant ATR, nucula
     vpcd slot presence (expected no-card until #554 lands).
  3. S20 phone: adb-reachable on small, NFC state.
  4. labgrid place holds (exclusive hardware ownership visibility).

Output: human table (default) or --json. Exit code is always 0 — this
is a reporting instrument; gates belong to CI, and several of these
rows carry known, tracked gaps on purpose.

Run (from repo root, on ai-legion):
  python3 tests/hardware/nfc/fixture_selfcheck.py [--json]
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys

SMALL = "root@ai-legion-small"
LANE_SOCK = "/run/pcscd-lanes/m5stick.sock"
S20_SERIAL = "RF8NB01ZYAH"

# ATR signatures measured on the bench (2026-11-03 session, probed).
# Prefix-matched: readers synthesize ATRs with variable check bytes.
ATR_SIGNATURES = {
    "S20-HCE-m5stick": "3b808131fe458b",
    "S20-HCE-ACR": "3b888001000000008081710079",
    "NTAG424-04c474fa967380": "3b8180018080",
    "JCOP-50942720-keyed": "3b8580018073c821100e",
    "JCOP-50947320-locked": "3b858131fe458073c8211084",
}

# Expected fixture state — the standing rig (update when the layout
# deliberately changes; the selfcheck's job is to catch UNINTENDED
# drift + dead lanes, see SEATS.md).
EXPECTED = {
    "m5stick-coil": "S20-HCE-m5stick",
    "acr-coil": "NTAG424-04c474fa967380",
    # both slots report no-card even with a coupled card — #554; the
    # row DRIFTs (loudly) the day that bug is fixed and we forget to
    # update this file, which is exactly when we want to look.
    "nucula-vpcd-slots": "no-card no-card",
}


def ssh_small(script: str) -> str:
    proc = subprocess.run(["ssh", "-o", "ConnectTimeout=5", SMALL, script],
                          capture_output=True, text=True, timeout=60)
    return (proc.stdout or "") + (proc.stderr or "")


def pcsc_probe(env_sock: str | None, needle: str, script: str) -> dict:
    """Run a pcsc probe script; return {'ok', 'out'}."""
    prefix = [f"PCSCLITE_CSOCK_NAME={env_sock}"] if env_sock else []
    cmd = " ".join(prefix) + f" /usr/bin/python3 - <<'PYE'\n{script}\nPYE"
    proc = subprocess.run(["bash", "-c", cmd], capture_output=True,
                          text=True, timeout=90)
    out = (proc.stdout or "").strip()
    return {"ok": proc.returncode == 0, "out": out}


def probe_reader(env_sock: str | None, needle: str) -> dict:
    script = f"""
from smartcard.System import readers
from smartcard.Exceptions import NoCardException
matched = [x for x in readers() if "{needle}" in str(x)]
if not matched:
    print("READER-ABSENT")
else:
    try:
        c = matched[0].createConnection(); c.connect()
        print("ATR " + bytes(c.getATR()).hex())
        c.disconnect()
    except NoCardException:
        print("NO-CARD")
    except Exception as e:
        print("CONNECT-FAIL " + type(e).__name__)
"""
    res = pcsc_probe(env_sock, needle, script)
    out = res["out"]
    if out.startswith("ATR "):
        return {"state": "card", "atr": out[4:].strip()}
    if out == "NO-CARD":
        return {"state": "no-card", "atr": None}
    if out == "READER-ABSENT":
        return {"state": "reader-absent", "atr": None}
    return {"state": out.split()[0] if out else "dead", "atr": None}


def classify(atr: str | None) -> str | None:
    if not atr:
        return None
    for name, sig in ATR_SIGNATURES.items():
        if atr.startswith(sig) or atr == sig:
            return name
    return f"unknown({atr})"


def check_all() -> list:
    rows = []

    def row(fixture, expect, state, detail):
        ok = "OK" if expect == state else "DRIFT"
        rows.append({"fixture": fixture, "expected": expect, "actual": state,
                     "status": ok, "detail": detail})

    m5 = probe_reader(LANE_SOCK, "M5stick")
    occ = classify(m5["atr"]) or m5["state"]
    row("m5stick-coil (.208 lane pcscd)", EXPECTED["m5stick-coil"], occ,
        m5.get("atr") or "")

    acr_remote = ssh_small(
        '/usr/bin/python3 -c "\n'
        'from smartcard.System import readers\n'
        'from smartcard.Exceptions import NoCardException\n'
        'm=[x for x in readers() if \'ACR1252 Dual Reader PICC\' in str(x)]\n'
        'try:\n'
        '    c=m[0].createConnection(); c.connect(); print(\'ATR \'+bytes(c.getATR()).hex()); c.disconnect()\n'
        'except NoCardException: print(\'NO-CARD\')\n'
        'except Exception as e: print(\'FAIL \'+type(e).__name__)\n"')
    acr_state = acr_remote.strip()
    if acr_state.startswith("ATR "):
        acr_occ = classify(acr_state[4:].strip())
    else:
        acr_occ = acr_state or "dead"
    row("acr-coil (small system pcscd)", EXPECTED["acr-coil"], acr_occ,
        acr_state)

    nuc = ssh_small(
        '/usr/bin/python3 -c "\n'
        'from smartcard.System import readers\n'
        'from smartcard.Exceptions import NoCardException\n'
        'n=[x for x in readers() if \'Nucula\' in str(x)]\n'
        'p=[]\n'
        'for x in n:\n'
        '    try:\n'
        '        c=x.createConnection(); c.connect(); p.append(str(x).split()[-2:][0]+\'=CARD\'); c.disconnect()\n'
        '    except Exception: p.append(\'no-card\')\n'
        'print(\' \'.join(p) or \'slots-absent\')\n"').strip() or "dead"
    row("nucula-vpcd-slots (small)", EXPECTED["nucula-vpcd-slots"],
        nuc if "CARD" not in nuc else f"CARD?! {nuc}", nuc)

    adb = ssh_small(f"adb -s {S20_SERIAL} get-state 2>&1").strip()
    nfc = ssh_small(
        f'adb -s {S20_SERIAL} shell "dumpsys nfc 2>/dev/null | grep -m1 State"'
    ).strip()
    row("S20 adb (small)", "device", adb, nfc)

    who = subprocess.run(["labgrid-client", "who"], capture_output=True,
                         text=True, timeout=30).stdout or ""
    nfc_places = ("m5stick", "nucula", "phone-", "ref-acr")
    held = [ln.split()[2] for ln in who.splitlines()[1:]
            if len(ln.split()) >= 3]
    nfc_held = [p for p in held if any(k in p for k in nfc_places)]
    row("labgrid NFC places", "none-held",
        ",".join(nfc_held) if nfc_held else "none-held",
        f"(all holds: {','.join(held) or 'none'})")
    return rows


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()
    rows = check_all()
    if args.json:
        print(json.dumps(rows, indent=2))
        return 0
    width = max(len(r["fixture"]) for r in rows)
    for r in rows:
        mark = "==" if r["status"] == "OK" else "!!"
        print(f"{mark} {r['fixture']:<{width}}  expect={r['expected']}")
        print(f"   {'':<{width}}  actual={r['actual']}  {r['detail'][:70]}")
    drift = [r for r in rows if r["status"] != "OK"]
    print(f"\n{len(rows) - len(drift)}/{len(rows)} fixtures match expected;"
          f" {len(drift)} drift")
    return 0


if __name__ == "__main__":
    sys.exit(main())
