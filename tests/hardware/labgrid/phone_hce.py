#!/usr/bin/env python3
"""phone_hce.py — phones as virtual cards, with labgrid verbs (#529).

Bench-proven primitives (2026-10-31, all live on the S20):
  svc nfc disable/enable  = place/remove card (~10 s out, ~30 s back —
  the Samsung NFC stack restart; field cycle also clears the burned
  ISO-DEP session after any app/routing change, the #529 lesson);
  pm enable/disable-user  = HCE app selection (multi-HCE phones route
  among services — disabling one does NOT remove the card);
  dumpsys nfc             = routing/registration snapshot;
  adb forward + the relay app (host-tools/hce-relay-app) = host-driven
  card brain.

Two faces:
  CLI (no labgrid needed):
    python3 phone_hce.py --serial RF8NB01ZYAH status
    python3 phone_hce.py place-card [--profile relay|boltcard|plain]
    python3 phone_hce.py remove-card
    python3 phone_hce.py await-presence --reader-needle M5stick
  labgrid driver (binds the USBDevice the phone already exports; the
    adb serial rides as a driver arg — no exporter change needed):
    imports: [phone_hce] in the place YAML, then under drivers
    PhoneHceDriver: {serial: "RF8NB01ZYAH"} — exposes place_card() /
    remove_card() / status() / await_presence() on the target.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import time
from typing import Optional

# --- adb plumbing (cashu-nfc-matrix drivers/ui.py pattern: hang-proof,
# stdout to file, whole process group killed on timeout) -------------------


class AdbError(RuntimeError):
    pass


def adb(serial: Optional[str], *args: str, timeout: float = 30) -> str:
    cmd = ["adb"]
    if serial:
        cmd += ["-s", serial]
    cmd += list(args)
    try:
        proc = subprocess.run(
            cmd, capture_output=True, text=True, timeout=timeout,
            start_new_session=True)
    except subprocess.TimeoutExpired:
        raise AdbError(f"adb {args[:3]} timed out after {timeout}s") from None
    out = (proc.stdout or "") + (proc.stderr or "")
    if proc.returncode != 0:
        raise AdbError(f"adb {args} rc={proc.returncode}: {out.strip()[:200]}")
    return (proc.stdout or "").strip()


# --- the verbs --------------------------------------------------------------


class PhoneHce:
    """The #529 place_card/remove_card/status verbs over adb."""

    # App churn burns the live ISO-DEP session (#529): after ANY change
    # around the HCE stack, presence must be awaited through a field
    # cycle rather than assumed.
    FIELD_CYCLE_SETTLE_S = 40.0

    def __init__(self, serial: Optional[str] = None):
        self.serial = serial

    def _shell(self, *args: str, timeout: float = 30) -> str:
        return adb(self.serial, "shell", *args, timeout=timeout)

    def status(self) -> dict:
        """NFC state + HCE registrations + relay reachability."""
        out = {"serial": self.serial or "default"}
        nfc = self._shell("dumpsys", "nfc")
        out["nfc_state"] = next(
            (ln.split(":", 1)[1].strip() for ln in nfc.splitlines()
             if ln.startswith("State:")), "unknown")
        out["hce_services"] = sorted({
            ln.split("}")[0].split("{")[-1].strip()
            for ln in nfc.splitlines() if "ComponentInfo{" in ln
            and "benchrelay" not in ln})
        out["relay_registered"] = "benchrelay" in nfc
        out["payment_default"] = adb(
            self.serial, "shell", "settings", "get", "secure",
            "nfc_payment_default_component")
        out["screen_on"] = "ON_" in next(
            (ln for ln in nfc.splitlines() if "Screen State" in ln), "")
        try:
            adb(self.serial, "shell", "pidof", "com.amperstrand.benchrelay")
            out["relay_process"] = True
        except AdbError:
            out["relay_process"] = False
        return out

    def wake(self) -> None:
        """Screen awake + stay-on + battery-optimization exemption — the
        Samsung freezer/AppChooser lessons from #529 and the matrix."""
        self._shell("input", "keyevent", "KEYCODE_WAKEUP")
        self._shell("svc", "power", "stayon", "true")
        # idempotent; fails harmlessly where unsupported
        try:
            self._shell("dumpsys", "deviceidle", "whitelist",
                        "+com.amperstrand.benchrelay")
        except AdbError:
            pass

    def place_card(self, profile: str = "plain",
                   settle: bool = True) -> dict:
        """Card PRESENT on the coil. Profiles:
          plain    — NFC on, whatever HCE routing is configured
          boltcard — the NTAG424 boltcard HCE apps enabled (matrix soaks)
          relay    — the benchrelay app kicked + payment default taken
                     (host brain must run hce_relay.py)
        settle=True waits out the field cycle (the burned-session rule)."""
        self.wake()
        if profile == "boltcard":
            for pkg in ("dev.boltcard.hce", "com.amperstrand.boltcard.hce"):
                try:
                    self._shell("pm", "enable", pkg)
                except AdbError:
                    pass
        elif profile == "relay":
            self._shell("pm", "enable", "com.amperstrand.benchrelay")
            self._shell("am", "start-foreground-service", "-n",
                        "com.amperstrand.benchrelay/.RelayKickstartService")
            adb(self.serial, "forward", "tcp:4871",
                "localabstract:bench-hce-relay")
            self._shell("settings", "put", "secure",
                        "nfc_payment_default_component",
                        "com.amperstrand.benchrelay/"
                        "com.amperstrand.benchrelay.RelayApduService")
        self._shell("svc", "nfc", "enable")
        if settle:
            time.sleep(self.FIELD_CYCLE_SETTLE_S)
        return self.status()

    def remove_card(self) -> dict:
        """Card ABSENT — the definitive verb (multi-HCE routing means pm
        disable does NOT remove the card; #529)."""
        self._shell("svc", "nfc", "disable")
        return self.status()

    def await_presence(self, reader_needle: str,
                       want: bool = True, timeout: float = 90.0) -> bool:
        """Verify the card's presence/absence through a real pcscd
        reader (the checklist's 'presence verification via reader').
        pyscard imported lazily — never at module scope of a collected
        tree (python-collect rule)."""
        from smartcard.System import readers  # noqa: PLC0415
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                matches = [r for r in readers() if reader_needle in str(r)]
                if not matches:
                    raise AdbError(f"no pcscd reader matching {reader_needle!r}")
                conn = matches[0].createConnection()
                present = True
                try:
                    conn.connect()
                except Exception:  # noqa: BLE001 — absent is a state
                    present = False
                if present == want:
                    return True
            except Exception:  # noqa: BLE001 — transient pcscd states
                pass
            time.sleep(3)
        return False


# --- labgrid glue (optional; the CLI works without it) ----------------------
#
# labgrid 26.0 (this bench install) ships no adb classes — this is the
# first adb-based driver on the install. It binds the plain USBDevice
# the phone already exports (ai-legion-nfc/phone-s20/USBDevice) and
# takes the adb serial as a driver arg, so NO exporter change is needed:
#
#   imports: [phone_hce]     # resolves via tests/hardware/labgrid on sys.path
#   drivers:
#     PhoneHceDriver:
#       serial: "RF8NB01ZYAH"

try:
    import attr
    from labgrid.driver import Driver
    from labgrid.factory import target_factory

    @target_factory.reg_driver
    @attr.s(eq=False)
    class PhoneHceDriver(Driver):
        """labgrid face: place_card/remove_card/status/await_presence."""

        bindings = {"phone": "USBDevice"}
        serial = attr.ib(default=None)

        def __attrs_post_init__(self) -> None:
            super().__attrs_post_init__()
            self._hce: PhoneHce | None = None

        def on_activate(self) -> None:
            self._hce = PhoneHce(self.serial)

        def place_card(self, profile: str = "plain") -> dict:
            assert self._hce is not None
            return self._hce.place_card(profile)

        def remove_card(self) -> dict:
            assert self._hce is not None
            return self._hce.remove_card()

        def status(self) -> dict:
            assert self._hce is not None
            return self._hce.status()

        def await_presence(self, reader_needle: str,
                           want: bool = True, timeout: float = 90.0) -> bool:
            assert self._hce is not None
            return self._hce.await_presence(reader_needle, want, timeout)

except ImportError:  # labgrid not installed — CLI-only face
    PhoneHceDriver = None  # type: ignore[assignment,misc]


# --- CLI ---------------------------------------------------------------------


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--serial", default=None, help="adb device serial")
    sub = ap.add_subparsers(dest="verb", required=True)
    sub.add_parser("status")
    p_place = sub.add_parser("place-card")
    p_place.add_argument("--profile", default="plain",
                         choices=("plain", "boltcard", "relay"))
    p_place.add_argument("--no-settle", action="store_true")
    sub.add_parser("remove-card")
    p_await = sub.add_parser("await-presence")
    p_await.add_argument("--reader-needle", required=True)
    p_await.add_argument("--absent", action="store_true",
                         help="await ABSENCE instead of presence")
    p_await.add_argument("--timeout", type=float, default=90.0)
    args = ap.parse_args(argv)

    hce = PhoneHce(args.serial)
    if args.verb == "status":
        for k, v in hce.status().items():
            print(f"{k}: {v}")
        return 0
    if args.verb == "place-card":
        st = hce.place_card(args.profile, settle=not args.no_settle)
        print("placed:", st["nfc_state"])
        return 0
    if args.verb == "remove-card":
        print("removed:", hce.remove_card()["nfc_state"])
        return 0
    ok = hce.await_presence(args.reader_needle,
                            want=not args.absent, timeout=args.timeout)
    print("presence:", "CONFIRMED" if ok else "TIMEOUT")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
