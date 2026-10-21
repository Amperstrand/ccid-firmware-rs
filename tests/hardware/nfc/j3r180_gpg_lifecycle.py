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
import os
import sys
import hashlib
import subprocess
import time
from pathlib import Path

# --- org single source of truth (history: tests/soak/soak_02_globalplatform) ---
AUTH_KEYS = {
    "enc": "5A9E63D03BADBC2A240FE8F534709EDF",
    "mac": "7CCC1E79D64FC5FA263B8F2955282998",
    "dek": "B040703EC3DE23EE8AE4CFB6D632AA80",
}
J3R180_ATR_PREFIXES = ("3B8580", "3B8581", "3BD518FF")  # ATR is
# negotiated per-reader AND per-interface: ACR1252 answers 3B 85 80 01
# 80 …, the m5stick MFRC522 3B 85 81 31 FE 45 … (contactless), and the
# F469 reads the same card family over its CONTACT pads as
# 3B D5 18 FF 81 91 FE … (all share the NXP 73 C8 21 tail).
OPENPGP_SELECT = "00A4040008D276000124010200"  # v2 AID, 8 bytes
# OUR package AID (the ss3t CAP gp reports on load). Cleanup treats
# ONLY artifacts under this package as ours; foreign D276000124*
# packages are PRE-EXISTING owner applets (the nucula coil card ships
# a real OpenPGP v3.4 applet D276000124010304AFAF… — plus "Satocash"
# — never deleted, reported instead).
OUR_PKG_AID = "D2760001240002000000000000000000"
# Applet CAP: SmartPGP v1.23.0, javacard 3.0.4 (JCOP4-native), RSA to
# 4096 — the gpg-proven applet from the wifi-vpcd e2e suite (which ran
# the delete->install->gpg cycle twice). Bench path (caps are never
# committed); override with --cap. The 9KB FluffyKaon ss3t CAP loads
# on JCOP4 but is NOT functionally complete there (6A88/6E00) — kept
# only as a documented fallback.
SMARTPGP_CAP = Path("/root/src/ccid-firmware-rs/.omo/gp-artifacts/"
                    "SmartPGP-v1.23.0-javacard-3.0.4/"
                    "SmartPGPApplet-rsa_up_to_4096.cap")
FALLBACK_CAP = Path("/root/hil-stm32-redo/j3r180-gpg/gpg.cap")

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



# #332: armed after a successful install; the crash handler rolls the
# card back to baseline instead of leaving artifacts on an exception.
_ROLLBACK: dict = {"reader": None, "cap": None, "armed": False}

ALLOW_UNVERIFIED_CPLC = [False]  # set from argv in main()


def plain_cplc(reader: str) -> str | None:
    """Card CPLC via plain GET DATA (no GP auth) — None if unreadable."""
    try:
        from smartcard.pcsc.PCSCCardConnection import PCSCCardConnection
        from smartcard.util import toBytes
        cc = PCSCCardConnection(reader)
        try:
            cc.connect()
            # Codex P1 on #341: GET DATA CPLC is case-2 SHORT Le — the
            # four-byte case-1 form is malformed and cards reject it,
            # making plain_cplc() always-None and every ledger-protected
            # run abort.
            data, s1, s2 = cc.transmit(toBytes("80CA9F7F00"))
        finally:
            try:
                cc.disconnect()
            except Exception:
                pass
        if s1 != 0x90 or not data:
            return None
        raw = bytes(data)
        # Codex P1 on #341: the response is BER-TLV — 9F7F <len> <42B
        # CPLC value>. Ledger entries pin the CPLC VALUE hex (e.g.
        # "47909E..." fabricator/ROM first), so unwrap before compare.
        if len(raw) >= 2 and raw[0] == 0x9F and raw[1] == 0x7F:
            body = raw[2:]
            if len(body) < 1:
                return None
            ln = body[0]
            if len(body) - 1 < ln:
                return None
            return body[1:1 + ln].hex()
        # Unexpected response shape (not the 9F7F BER-TLV we asked
        # for) is UNREADABLE, not "compare the raw bytes" — a garbage
        # answer must never reach the ledger compare (fail closed).
        return None
    except Exception:
        return None


# GP Card Spec (v2.3 §11.9 / Table 11-86) CPLC '9F7F' value layout:
# ICS fabricator(2) ICS type(2) OS id(2) OS date(2) OS level(2)
# ICF fabricator(1) ICF type(1) ICF serial(4) — the ICF serial ends at
# byte 16, so an identity-level pin needs >= 32 hex chars; the ICF
# batch number follows (batch kinship). A batch-scoped entry must SAY
# so with a trailing '*' (operator-accepted weaker guarantee).
CPLC_IDENTITY_MIN_HEX = 32  # bytes 0..15: fabricator .. ICF serial


def cplc_ledger_verdict(cplc_hex: str | None, ledger: list[str],
                        allow_unverified: bool) -> tuple[bool, str]:
    """Pure gate: may the fixed org keyset be used on this card?

    Fail-closed on: no ledger, malformed/under-length ledger entries,
    unreadable CPLC (unless allow_unverified), unregistered identity.
    Returns (proceed, log-line)."""
    if not ledger:
        return False, ("preflight FAIL: J3R180_CPLC_ALLOWLIST unset — "
                       "refusing GP auth with the fixed keyset (pin the "
                       "card's CPLC value; an ATR prefix is family, not "
                       "identity)")
    if cplc_hex is None:
        if allow_unverified:
            return True, ("WARN: CPLC unreadable; proceeding on "
                          "--allow-unverified-cplc (operator call)")
        return False, ("preflight FAIL: CPLC unreadable — refusing GP "
                       "auth with the fixed keyset (pin the card or "
                       "pass --allow-unverified-cplc)")
    cplc = cplc_hex.upper()
    hexdigits = set("0123456789ABCDEF")
    # Pass 1 — validate EVERY entry before any match: a malformed
    # ledger is a configuration error and must refuse everything,
    # even when another entry would have matched.
    for raw in ledger:
        entry = raw.strip().upper()
        if not entry:
            continue
        if entry.endswith("*"):
            pfx = entry[:-1].strip()
            if len(pfx) < 6 or not set(pfx) <= hexdigits:
                return False, (f"preflight FAIL: batch entry '{raw}' is "
                               "not hex/a usable prefix")
            continue
        if len(entry) < CPLC_IDENTITY_MIN_HEX \
                or not set(entry) <= hexdigits:
            return False, (f"preflight FAIL: ledger entry '{raw}' is not "
                           "an identity pin — need >= "
                           f"{CPLC_IDENTITY_MIN_HEX} hex chars (through "
                           "the GP CPLC ICF serial) or an explicit "
                           "'*' batch prefix")
    # Pass 2 — match.
    for raw in ledger:
        entry = raw.strip().upper()
        if not entry:
            continue
        if entry.endswith("*") and cplc.startswith(entry[:-1].strip()):
            return True, (f"preflight: CPLC {cplc[:16]}… matches declared "
                          f"batch prefix {entry[:-1]} (operator-scoped, "
                          "weaker than identity)")
        if not entry.endswith("*") and cplc.startswith(entry):
            return True, f"preflight: CPLC {cplc[:16]}… ledgered ✓"
    return False, (f"preflight FAIL: CPLC {cplc[:16]}… not in the "
                   "allowlist — unknown card, refusing GP auth")


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
    # --bs 200: APDUs >~245B fail SW 6400 on the bench card path (the
    # wifi-vpcd e2e suite's proven recipe); without it gp's oversized
    # blocks fail and retry at a crawl — the 2026-10-20 "slow LOAD"
    # was largely THIS, not the transport.
    cmd = ["gp", "-r", reader,
           "--key-enc", AUTH_KEYS["enc"],
           "--key-mac", AUTH_KEYS["mac"],
           "--key-dek", AUTH_KEYS["dek"],
           "--bs", "200"] + sub.split()
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
    # 5. org keyset authenticates (ISD reachable) — read-only gp -l.
    # Codex P1 on #325: gated on the ATR check — authenticating with
    # the fixed org keyset against a NON-J3R180 card burns its auth
    # budget (gp warns re-tries can brick); a failed ATR check must
    # stop the run before any key-bearing exchange.
    if atr is None or not any(atr.upper().startswith(pfx)
                              for pfx in J3R180_ATR_PREFIXES):
        lines.append("preflight FAIL: skipping GP auth — ATR family "
                     "check failed (wrong-card key-burn guard)")
        return False
    # Codex P1 on #333: an ATR prefix proves FAMILY, not identity. The
    # fixed org keyset goes ONLY to ledgered cards: every entry in
    # J3R180_CPLC_ALLOWLIST pins either the card's CPLC value at
    # identity level (>= 32 hex chars, through the GP CPLC ICF serial)
    # or an explicit '*' batch prefix. A MISSING ledger refuses GP
    # auth outright — the ATR-family gate alone is not enough (mission
    # card-safety hardening on #341).
    ledger = [s.strip() for s
              in os.environ.get("J3R180_CPLC_ALLOWLIST",
                                "").split(",") if s.strip()]
    proceed, verdict_line = cplc_ledger_verdict(
        plain_cplc(reader), ledger, ALLOW_UNVERIFIED_CPLC[0])
    lines.append(verdict_line)
    if not proceed:
        return False
    r = gp(reader, "-l")
    check("GlobalPlatform ISD authenticates with org keyset",
          r.returncode == 0 and "ISD:" in r.stdout,
          (r.stderr or r.stdout).strip().splitlines()[-1][:80]
          if (r.stderr or r.stdout).strip() else "")
    return ok




def baseline_pkgs(reader: str) -> list[str] | None:
    """Package registry, or None when the QUERY itself failed (#332,
    Codex on #325: a transient empty `gp -l` after heavy traffic was
    silently read as '0 pkgs' — a clean card never has zero packages,
    the ISD row always exists)."""
    r = gp(reader, "-l")
    if r.returncode != 0 or "ISD:" not in r.stdout:
        return None
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
            if ln.startswith("PKG:") and (
                    ln.split()[1].upper().startswith("D276000124")
                    or ln.split()[1].upper().startswith("5361746F63")):
                found.append(ln.split()[1])
            if ln.strip().startswith("Applet:"):
                aid = ln.strip().split()[1]
                # owner-approved full wipe (2026-10-20): all OpenPGP
                # instances + SmartPGP/Satocash artifacts
                if aid.upper().startswith(("D276000124", "5361746F63")):
                    found.append(aid)
        return found

    pre = artifacts()
    if not pre:
        return True  # already clean — nothing to do is success
    if cap is not None and cap.exists():
        gp(reader, f"--uninstall {cap}")
    for aid in artifacts():
        gp(reader, f"--delete {aid}")
    for aid in artifacts():  # privileged strays: force
        gp(reader, f"--delete {aid} --force")
    return not artifacts()


def status_only_exit(lines: list[str]) -> int:
    """--status-only must FAIL when its checks fail (#332, Codex on
    #325): a probe that always exits 0 is invisible to CI/cron."""
    return 7 if any("FAIL" in ln for ln in lines) else 0


def main() -> int:
    ALLOW_UNVERIFIED_CPLC[0] = "--allow-unverified-cplc" in sys.argv
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--reader", required=True,
                    help="EXACT pcscd reader name (never an index)")
    ap.add_argument("--allow-unverified-cplc", action="store_true",
                    help="proceed when the CPLC is unreadable but a ledger is set")
    ap.add_argument("--status-only", action="store_true",
                    help="read-only: preflight + SELECT + gpg status")
    ap.add_argument("--expect-cap-sha256", default=None,
                    help="pin the CAP digest (or J3R180_CAP_SHA256 env); "
                         "a mismatch aborts before install")
    ap.add_argument("--cap", default=None,
                    help="applet CAP (default: bench SmartPGP jc3.0.4)")
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

    # Codex P1 on #325: --status-only is READ-ONLY — the destructive
    # cleanup (owner-approved full wipes!) must not run in that mode.
    if args.status_only:
        lines.append("[status-only] cleanup SKIPPED (read-only mode)")
    # Codex P2 on #333: --status-only is a read-only probe — it must
    # not REQUIRE an applet CAP (cleanup is skipped in that mode, so
    # the CAP is never used; non-bench machines have neither path).
    cap: Path | None = Path(args.cap) if args.cap else (
        SMARTPGP_CAP if SMARTPGP_CAP.exists() else FALLBACK_CAP)
    if args.status_only:
        cap = None
        lines.append("[status-only] CAP not required (read-only mode)")
    elif not cap.exists():
        lines.append(f"FAIL: no applet CAP (looked for {SMARTPGP_CAP} "
                     f"and {FALLBACK_CAP}; pass --cap)")
        return 3
    if cap is not None:
        digest = hashlib.sha256(cap.read_bytes()).hexdigest()
        lines.append(f"# applet CAP sha256={digest} (pin this in the "
                     "runbook after first green run)")
        # #332 (Codex on #325): enforce a pinned digest when provided —
        # a corrupted/replaced cache must never reach an install.
        want = (args.expect_cap_sha256
                or os.environ.get("J3R180_CAP_SHA256", ""))
        if want and want.lower() != digest:
            lines.append(f"FAIL: CAP sha256 {digest} != pinned {want} — "
                         "refusing to install an unverified artifact")
            print("\n".join(lines))
            return 3

    # --- idempotency: cleanup BEFORE baseline so the captured resting
    # state is the card's true baseline (not baseline+strays).
    # Codex P1 on #325: a FAILED cleanup ABORTS — installing on top of
    # undeletable strays produces mixed-state cards.
    if not args.status_only:
        if not cleanup_openpgp(args.reader, cap):
            phase("cleanup pre-existing OpenPGP artifacts", False,
                  "ABORT: undeletable strays — refusing to install over them")
            print("\n".join(lines))
            return 5
        phase("cleanup pre-existing OpenPGP artifacts", True)

    baseline = baseline_pkgs(args.reader)
    if baseline is None:
        phase("baseline registry captured", False,
              "ABORT: registry query failed (gp -l) — not a card state")
        print("\n".join(lines))
        return 5
    phase("baseline registry captured", bool(baseline),
          f"{len(baseline)} pkgs")

    if not args.status_only:
        r = gp(args.reader, f"-install {cap}", timeout=900)
        install_ok = r.returncode == 0 and "loaded" in r.stdout.lower()
        phase("install OpenPGP applet", install_ok,
              r.stdout.strip().splitlines()[-1][:70] if r.stdout else "")
        # #332 (Codex on #325): mid-lifecycle crashes must not leave the
        # installed artifacts behind — arm the rollback the __main__
        # crash handler honors.
        if install_ok:
            _ROLLBACK.update(reader=args.reader, cap=cap, armed=True)
        if not install_ok:
            # #332 (Codex on #325): a failed install leaves partial
            # artifacts — roll back to baseline and ABORT (Codex P1,
            # 00:12: falling through probed a half-loaded package).
            phase("rollback after failed install",
                  cleanup_openpgp(args.reader, cap))
            _ROLLBACK["armed"] = False
            print("\n".join(lines))
            return 8

    # --- the card answers OpenPGP protocol on THIS reader (direct APDUs,
    # deterministic — no scdaemon dependency; every exchange bounded) ---
    from smartcard.pcsc.PCSCCardConnection import PCSCCardConnection
    from smartcard.util import toBytes

    # #330 (cause 2): ONE connection for the whole probe phase — a
    # fresh pcscd connection per APDU powers the card down on
    # last-disconnect (the MFRC522 path honors it), dropping the
    # applet selection; the SELECT and the data commands must share a
    # card session, exactly like a real application.
    probe_cc = PCSCCardConnection(args.reader)
    probe_cc.connect()

    def apdu(hexcmd: str) -> tuple[list, int, int]:
        return probe_cc.transmit(toBytes(hexcmd))

    # SELECT: generic v2 AID, then the card's ACTUAL instance AID
    # (discovered from the registry — a v3.4 instance does not answer
    # the v2 prefix; the ss3t fallback does not answer the v3 one)
    r = gp(args.reader, "-l")
    instance_aids = [ln.strip().split()[1]
                     for ln in r.stdout.splitlines()
                     if ln.strip().startswith("Applet:")
                     and ln.strip().split()[1].upper().startswith("D276")]
    sel_ok, sel_detail = False, "no instance"
    for hexsel in ([OPENPGP_SELECT] +
                   [f"00A40400{len(a)//2:02X}{a}" for a in instance_aids]):
        data, sw1, sw2 = apdu(hexsel)
        if sw1 == 0x90:
            sel_ok, sel_detail = True, f"SW=9000 via {hexsel[10:26]}"
            break
        sel_detail = f"SW={sw1:02X}{sw2:02X}"
    phase("OpenPGP SELECT via pcscd", sel_ok, sel_detail)

    # Applet-function findings: the reader-stack contract is load/
    # select/uninstall/restore; whether THIS applet build answers
    # OpenPGP data commands is a property of the CAP (the JC2.2.2-era
    # ss3t CAP loads on JCOP4 but answers 6A88/6E00 — documented,
    # machinery unaffected). Findings, not FAILs.
    data, sw1, sw2 = apdu("00CA006E")     # GET DATA: application info
    lines.append(f"[{time.time()-t0:6.1f}s] GET DATA 6E: "
                 f"{'answered' if sw1 == 0x90 else 'finding'} — "
                 f"SW={sw1:02X}{sw2:02X} len={len(data)}")

    # Codex P1 on #333: NEVER send VERIFY in --status-only — a
    # preserved applet with a NON-default PIN would count failed
    # retries against its retry counter (three strikes block PW1).
    if args.status_only:
        lines.append(f"[{time.time()-t0:6.1f}s] VERIFY PW1: SKIPPED "
                     "(status-only — PIN retry budget untouched)")
    else:
        data, sw1, sw2 = apdu("0020008106" + "313233343536")
        lines.append(f"[{time.time()-t0:6.1f}s] VERIFY PW1: "
                     f"{'answered' if sw1 == 0x90 else 'finding'} — "
                     f"SW={sw1:02X}{sw2:02X}")

    data, sw1, sw2 = apdu("00CA005E")
    lines.append(f"[{time.time()-t0:6.1f}s] GET DATA 5E: "
                 f"{'answered' if sw1 == 0x90 else 'finding'} — "
                 f"SW={sw1:02X}{sw2:02X}")

    try:
        probe_cc.disconnect()
    except Exception:
        pass

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
        # #332 (Codex on #325): status-only must FAIL when its checks
        # fail — a probe that always exits 0 is invisible to CI/cron.
        failed = [l for l in lines if "FAIL" in l]
        return 7 if failed else 0

    # --- uninstall + verify baseline restored ---
    _ROLLBACK["armed"] = False  # explicit uninstall supersedes rollback
    phase("uninstall OpenPGP artifacts", cleanup_openpgp(args.reader, cap))
    now = baseline_pkgs(args.reader)
    if now is None:
        phase("baseline registry restored", False,
              "registry query failed on verify — rerun `gp -l` before "
              "trusting the card state")
        print("\n".join(lines))
        return 6
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
        if _ROLLBACK["armed"]:
            print(f"ROLLBACK: removing installed artifacts on "
                  f"{_ROLLBACK['reader']}…")
            try:
                ok = cleanup_openpgp(_ROLLBACK["reader"], _ROLLBACK["cap"])
                if not ok:
                    print("ROLLBACK: FAILED — card needs manual cleanup")
                else:
                    # Codex P1 (00:12): verify the REGISTRY QUERY before
                    # claiming success — a transient gp -l failure must
                    # not read as a restored baseline.
                    verify = baseline_pkgs(_ROLLBACK["reader"])
                    if verify is None:
                        print("ROLLBACK: cleanup reported OK but the "
                              "registry verify query FAILED — re-check "
                              "with a fresh gp -l before trusting state")
                    else:
                        print(f"ROLLBACK: OK — baseline restored "
                              f"({len(verify)} pkgs)")
            except Exception as rb:  # noqa: BLE001
                print(f"ROLLBACK CRASH: {rb!r} — card needs manual cleanup")
    raise SystemExit(code)
