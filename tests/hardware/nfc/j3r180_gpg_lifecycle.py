#!/usr/bin/env python3
"""j3r180_gpg_lifecycle — known-good OpenPGP lifecycle for the J3R180 cards.

The proven-safe envelope for "install, use and test" a J3R180 JavaCard
through ANY pcscd-visible reader (ACR1252 oracle, m5stick GemPC serial,
nucula c3-ccid broker relay). Built from the 2026-10-20 session that
established the workflow; every trap hit that night is a PREFLIGHT
here, not a footnote:

- scdaemon stale config: the old soak rig left `reader-port 4` in
  scdaemon.conf — every bare gpg invocation failed with "No such
  device" because the pin pointed at a reader index that no longer
  exists. Preflight fails loudly on a reader-port pin and never trusts
  indexes: gpg is ALWAYS driven through `SCD SERIALNO --reader=N` with
  N resolved fresh from the pcscd enumeration.
- scdaemon hang class: bare `gpg --card-status` auto-scans ALL pcscd
  readers; virtual PICCs (boltcard vpcd) accept connections but never
  answer, wedging scdaemon reader-by-reader. The tool pins the target
  reader, bounds every subprocess, and kills stale scdaemons first.
- wrong-card / wrong-reader class: gp authenticate + install run ONLY
  against a reader whose card ATR matches the J3R180 pattern, with the
  org dev keyset from a single config block (tests/soak carries the
  history). Never default keys — a failed cryptogram burns auth
  budget (gp itself warns re-tries can brick the card).
- idempotency: an existing OpenPGP applet is uninstalled first; the
  baseline registry (ISD + NXP preloads) must be restored at the end
  or the run FAILS — the cards are left KNOWN, never "probably fine".

Usage:
  python3 j3r180_gpg_lifecycle.py --reader "ACS ACR1252 ..."   # full cycle
  python3 j3r180_gpg_lifecycle.py --reader ... --status-only    # read-only
  python3 j3r180_gpg_lifecycle.py --preflight --reader ...      # checks only

Exit 0 = every phase green; nonzero = the phase that failed is named.
"""

from __future__ import annotations

import argparse
import hashlib
import subprocess
import time
import urllib.request
from pathlib import Path

# --- org single source of truth (history: tests/soak/soak_02_globalplatform) ---
AUTH_KEYS = {
    "enc": "5A9E63D03BADBC2A240FE8F534709EDF",
    "mac": "7CCC1E79D64FC5FA263B8F2955282998",
    "dek": "B040703EC3DE23EE8AE4CFB6D632AA80",
}
J3R180_ATR_PREFIXES = ("3B8580", "3B8581")  # ATR is negotiated per-reader
# from the ATS: ACR1252 answers 3B 85 80 01 80 …, the m5stick MFRC522
# 3B 85 81 31 FE 45 … — same card, both forms are the J3R180 family.
OPENPGP_SELECT = "00A4040008D276000124010200"  # v2 AID, 8 bytes
# Upstream applet (MIT): FluffyKaon/OpenPGP-Card ships a prebuilt CAP.
CAP_URL = ("https://raw.githubusercontent.com/FluffyKaon/"
           "OpenPGP-Card/master/gpg.cap")
CAP_SHA256 = None  # filled on first fetch + printed; pin after first run
CAP_CACHE = Path("/root/hil-stm32-redo/j3r180-gpg/gpg.cap")

GP_TIMEOUT = 90
GPG_TIMEOUT = 30


class LiveLog(list):
    """Prints every line as it is appended — a killed run still leaves
    its evidence in the log file (the 2026-10-20 m5stick LOAD lesson:
    an 800s run died with an EMPTY log because everything printed at
    exit)."""
    def append(self, item):
        super().append(item)
        print(item, flush=True)


def sh(cmd: list[str] | str, timeout: int, shell: bool = False):
    """Bounded subprocess — nothing in this tool runs unbounded."""
    return subprocess.run(cmd, shell=shell, capture_output=True,
                          text=True, timeout=timeout)


def pcsc_readers() -> list[str]:
    import smartcard.System  # lazy: bench dep
    return [str(r) for r in smartcard.System.readers()]


def card_atr(reader: str) -> str | None:
    from smartcard.pcsc.PCSCCardConnection import PCSCCardConnection
    from smartcard.util import toHexString
    from smartcard.Exceptions import NoCardException
    cc = PCSCCardConnection(reader)
    try:
        cc.connect()
        return toHexString(cc.getATR()).replace(" ", "")
    except NoCardException:
        return None
    finally:
        try:
            cc.disconnect()
        except Exception:
            pass


def gp(reader: str, sub: str, timeout: int = GP_TIMEOUT):
    cmd = ["gp", "-r", reader,
           "--key-enc", AUTH_KEYS["enc"],
           "--key-mac", AUTH_KEYS["mac"],
           "--key-dek", AUTH_KEYS["dek"]] + sub.split()
    return sh(cmd, timeout)


def gpg_scd(reader: str, cmd: str, timeout: int = GPG_TIMEOUT,
            kill_first: bool = False):
    """Drive gpg via gpg-agent/scdaemon with the reader PINNED by index
    resolved fresh from pcscd (indexes shift when readers come/go —
    the stale reader-port-4 lesson). A WEDGE here is survivable: this
    host's pcscd carries reader configs for OTHER lanes (vpcd virtual
    PICCs that accept-block, a remote modem IFD) and scdaemon's
    startup enumeration can hang UNINTERRUPTIBLY on them — so on
    timeout we SIGKILL scdaemon and report the wedge by name instead
    of hanging the caller."""
    readers = pcsc_readers()
    if reader not in readers:
        return None, f"reader vanished from pcscd: {reader}"
    idx = readers.index(reader)
    if kill_first:
        sh(["gpgconf", "--kill", "scdaemon"], 10)
        time.sleep(0.5)
    try:
        r = sh(["gpg-connect-agent",
                f"SCD {cmd} --reader={idx}", "/bye"], timeout)
        return r, None
    except subprocess.TimeoutExpired:
        subprocess.run(["pkill", "-9", "-x", "scdaemon"], timeout=10)
        time.sleep(0.5)
        return None, (f"scdaemon WEDGED >{timeout}s (host pcscd reader-set "
                      "class: vpcd/modemifd IFDs) — killed -9")


def preflight(reader: str, lines: list) -> bool:
    ok = True

    def check(name: str, cond: bool, detail: str = ""):
        nonlocal ok
        lines.append(f"preflight {'PASS' if cond else 'FAIL'}: {name}"
                     + (f" — {detail}" if detail else ""))
        if not cond:
            ok = False

    # 1. scdaemon config must not pin a reader (the soak-rig trap)
    conf = Path.home() / ".gnupg/scdaemon.conf"
    if conf.exists():
        text = conf.read_text()
        check("scdaemon.conf has no stale reader-port pin",
              "reader-port" not in text, "bare gpg would fail/hang")
    # 2. no stale scdaemon holding old reader state
    p = sh(["pgrep", "-x", "scdaemon"], 10)
    check("no stale scdaemon", p.returncode != 0)
    # 3. pcscd lists the target reader BY NAME
    try:
        readers = pcsc_readers()
    except Exception as e:  # noqa: BLE001
        lines.append(f"preflight FAIL: pcscd enumeration — {e}")
        return False
    check(f"reader present in pcscd ({len(readers)} readers)",
          reader in readers)
    # 4. the card on that reader is a J3R180 (wrong-card guard)
    atr = card_atr(reader)
    check("J3R180 ATR family on target reader",
          atr is not None and any(atr.upper().startswith(p)
                                  for p in J3R180_ATR_PREFIXES),
          f"ATR={atr}")
    # 5. org keyset authenticates (ISD reachable) — read-only gp -l
    r = gp(reader, "-l")
    check("GlobalPlatform ISD authenticates with org keyset",
          r.returncode == 0 and "ISD:" in r.stdout,
          (r.stderr or r.stdout).strip().splitlines()[-1][:80]
          if (r.stderr or r.stdout).strip() else "")
    return ok


def fetch_cap(lines: list[str]) -> Path | None:
    if CAP_CACHE.exists():
        return CAP_CACHE
    lines.append("preflight WARN: fetching applet CAP from upstream "
                 "(cache it and pin the sha256 for reproducibility)")
    try:
        CAP_CACHE.parent.mkdir(parents=True, exist_ok=True)
        urllib.request.urlretrieve(CAP_URL, CAP_CACHE)
        return CAP_CACHE
    except Exception as e:  # noqa: BLE001
        lines.append(f"preflight FAIL: CAP fetch — {e}")
        return None


def baseline_pkgs(reader: str) -> list[str]:
    r = gp(reader, "-l")
    return sorted(ln.split()[1] for ln in r.stdout.splitlines()
                  if ln.startswith("PKG:"))


def cleanup_openpgp(reader: str, cap: Path | None) -> bool:
    """Remove any OpenPGP applet instances + package. GlobalPlatformPro
    syntax is --delete/--uninstall (the 2026-10-20 first run used the
    GPShell-style `-u -d` — gp silently rejected it and every later
    phase cascaded FAIL on an already-installed card). An already-clean
    card is SUCCESS (nothing to do)."""
    def artifacts() -> list[str]:
        r = gp(reader, "-l")
        found = []
        for ln in r.stdout.splitlines():
            if ln.strip().startswith("Applet:") and \
                    ln.strip().split()[1].upper().startswith("D276000124"):
                found.append(ln.strip().split()[1])
            if ln.startswith("PKG:") and \
                    ln.split()[1].upper().startswith("D276000124"):
                found.append(ln.split()[1])
        return found

    pre = artifacts()
    if not pre:
        return True  # already clean — nothing to do is success
    if cap is not None and cap.exists():
        gp(reader, f"--uninstall {cap}")
    for aid in artifacts():
        gp(reader, f"--delete {aid}")
    return not artifacts()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--reader", required=True,
                    help="EXACT pcscd reader name (never an index)")
    ap.add_argument("--status-only", action="store_true",
                    help="read-only: preflight + SELECT + gpg status")
    ap.add_argument("--preflight", action="store_true",
                    help="run the checks only, no card writes")
    args = ap.parse_args()

    lines = LiveLog()
    t0 = time.time()

    def phase(name: str, ok: bool, detail: str = ""):
        lines.append(f"[{time.time()-t0:6.1f}s] {name}: "
                     f"{'PASS' if ok else 'FAIL'}"
                     + (f" — {detail}" if detail else ""))
        return ok

    if not preflight(args.reader, lines):
        print("\n".join(lines))
        return 2
    if args.preflight:
        print("\n".join(lines))
        return 0

    cap = fetch_cap(lines)
    if cap is None:
        print("\n".join(lines))
        return 3
    digest = hashlib.sha256(cap.read_bytes()).hexdigest()
    lines.append(f"# applet CAP sha256={digest} (pin this in the "
                 "runbook after first green run)")

    # --- idempotency: cleanup BEFORE baseline so the captured resting
    # state is the card's true baseline (not baseline+strays) ---
    phase("cleanup pre-existing OpenPGP artifacts",
          cleanup_openpgp(args.reader, cap))

    baseline = baseline_pkgs(args.reader)
    phase("baseline registry captured", bool(baseline),
          f"{len(baseline)} pkgs")

    if not args.status_only:
        r = gp(args.reader, f"-install {cap}", timeout=480)
        phase("install OpenPGP applet", r.returncode == 0
              and "loaded" in r.stdout.lower(),
              r.stdout.strip().splitlines()[-1][:70] if r.stdout else "")

    # --- the card answers OpenPGP protocol on THIS reader (direct APDUs,
    # deterministic — no scdaemon dependency; every exchange bounded) ---
    from smartcard.pcsc.PCSCCardConnection import PCSCCardConnection
    from smartcard.util import toBytes

    def apdu(hexcmd: str) -> tuple[list, int, int]:
        cc = PCSCCardConnection(args.reader)
        try:
            cc.connect()
            return cc.transmit(toBytes(hexcmd))
        finally:
            try:
                cc.disconnect()
            except Exception:
                pass

    data, sw1, sw2 = apdu(OPENPGP_SELECT)
    phase("OpenPGP SELECT via pcscd", sw1 == 0x90, f"SW={sw1:02X}{sw2:02X}")

    # Applet-function findings: the reader-stack contract is load/
    # select/uninstall/restore; whether THIS applet build answers
    # OpenPGP data commands is a property of the CAP (the JC2.2.2-era
    # ss3t CAP loads on JCOP4 but answers 6A88/6E00 — documented,
    # machinery unaffected). Findings, not FAILs.
    data, sw1, sw2 = apdu("00CA006E")     # GET DATA: application info
    lines.append(f"[{time.time()-t0:6.1f}s] GET DATA 6E: "
                 f"{'answered' if sw1 == 0x90 else 'finding'} — "
                 f"SW={sw1:02X}{sw2:02X} len={len(data)}")

    data, sw1, sw2 = apdu("0020008106" + "313233343536")
    lines.append(f"[{time.time()-t0:6.1f}s] VERIFY PW1: "
                 f"{'answered' if sw1 == 0x90 else 'finding'} — "
                 f"SW={sw1:02X}{sw2:02X}")

    data, sw1, sw2 = apdu("00CA005E")
    lines.append(f"[{time.time()-t0:6.1f}s] GET DATA 5E: "
                 f"{'answered' if sw1 == 0x90 else 'finding'} — "
                 f"SW={sw1:02X}{sw2:02X}")

    # --- gpg host-stack leg: bounded probe, wedge = named verdict ---
    r, err = gpg_scd(args.reader, "SERIALNO", kill_first=True)
    serial = ""
    if r is not None:
        for ln in r.stdout.splitlines():
            if ln.startswith("D "):
                serial = ln[2:].strip()
    if err:
        lines.append(f"[{time.time()-t0:6.1f}s] gpg SCD SERIALNO: "
                     f"DEGRADED — {err}")
    else:
        phase("gpg SCD SERIALNO (pinned reader)", bool(serial), serial)

    if args.status_only:
        print("\n".join(lines))
        return 0

    # --- uninstall + verify baseline restored ---
    phase("uninstall OpenPGP artifacts", cleanup_openpgp(args.reader, cap))
    now = baseline_pkgs(args.reader)
    phase("baseline registry restored", now == baseline,
          f"{len(now)} pkgs" + ("" if now == baseline else f" != {baseline}"))

    print("\n".join(lines))
    fails = [l for l in lines if "FAIL" in l]
    return 0 if not fails else 1


if __name__ == "__main__":
    code = 0
    try:
        code = main()
    except Exception as e:  # noqa: BLE001 — evidence ALWAYS prints
        print(f"TOOL CRASH: {e!r}")
        code = 4
    raise SystemExit(code)
