#!/usr/bin/env python3
"""seat_audit.py — labgrid/SEATS/USB bench-coordination audit.

Mechanizes the 2026-11-14 F469 collision lesson (two lanes, one board,
no announcement): a hold that exists in labgrid but not in SEATS, and a
USB device that no labgrid exporter claims, are both invisible to every
other lane until they collide.

Checks (report mode always exit 0; --gate exits 1 on FLAG):
  1. held-unannounced  — labgrid `who` holds with no SEATS.md mention
                         in the N most recent dated sections (default 5).
  2. usb-unclaimed     — USB devices on the bench hosts that no labgrid
                         exporter resource claims by VID:PID or serial
                         (the pyboard class). Known-benign allowlist for
                         hubs/host-internal devices.
  3. resource-unregistered — exporter SerialPort/USBDevice resources whose
                         USB identity is missing from conwrt-lab lab.yaml
                         (info-level: labgrid is truth for exclusivity,
                         lab.yaml is truth for inventory).

Usage:
  python3 seat_audit.py [--gate] [--hosts ai-legion,ai-legion-small]
  LG_COORDINATOR env selects the coordinator (default 192.168.13.208:20408).
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys

DEFAULT_COORDINATOR = "192.168.13.208:20408"
DEFAULT_HOSTS = ["ai-legion", "ai-legion-small"]
SEATS_PATH = "/root/bench/SEATS.md"
LAB_YAML = "/root/src/conwrt-lab/lab.yaml"
SEATS_LOOKBACK_SECTIONS = 5

# Host-internal / infrastructure devices that are on every bench host and
# nobody's DUT. Anything NOT here and NOT claimed by an exporter = FLAG.
BENIGN_USB_IDS = {
    "1d6b:",   # Linux Foundation root hubs / hubs
    "17ef:",   # Lenovo ITE (bench keyboard/touch controller)
    "0bda:",   # Realtek BT radio (host accessory)
}


def run(cmd, timeout=30):
    """subprocess.run with utf-8 decode; None on failure."""
    try:
        p = subprocess.run(cmd, capture_output=True, timeout=timeout)
        return p.stdout.decode(errors="replace") if p.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        return None


def labgrid(args):
    import os
    env = dict(os.environ)
    env.setdefault("LG_COORDINATOR", DEFAULT_COORDINATOR)
    try:
        p = subprocess.run(["labgrid-client"] + args, capture_output=True,
                           timeout=30, env=env)
        if p.returncode != 0:
            return None
        return p.stdout.decode(errors="replace")
    except (OSError, subprocess.TimeoutExpired):
        return None


def parse_who(who_text):
    """labgrid `who` table -> [(user, host, place, changed)]."""
    holds = []
    for line in who_text.splitlines():
        if line.startswith(("User", "-")) or not line.strip():
            continue
        parts = line.split()
        if len(parts) >= 4:
            holds.append((parts[0], parts[1], parts[2], parts[3]))
    return holds


def parse_seats_recent(path, sections=SEATS_LOOKBACK_SECTIONS):
    """The N most recent '## <date>' sections of SEATS.md as text."""
    try:
        text = open(path, encoding="utf-8", errors="replace").read()
    except OSError:
        return ""
    marks = [m.start() for m in re.finditer(r"^## ", text, re.M)]
    if not marks:
        return text
    marks.append(len(text))
    recent = [text[marks[i]:marks[i + 1]] for i in range(len(marks) - 1)]
    return "\n".join(recent[-sections:])


SYSFS_USB_SCRIPT = """for d in /sys/bus/usb/devices/*/; do
  [ -f "$d/idVendor" ] || continue
  v=$(cat "$d/idVendor"); p=$(cat "$d/idProduct")
  s=$(cat "$d/serial" 2>/dev/null); n=$(cat "$d/product" 2>/dev/null)
  printf '%s:%s|%s|%s\n' "$v" "$p" "$s" "$n"
done"""


def parse_usb_devices(host):
    """sysfs USB inventory -> [(vidpid, serial, product)].

    Remote hosts run the script via `ssh host bash -s` with the script
    on STDIN — no shell=True, no quoting layers (the classic ssh+bash -c
    double-parse footgun), per subprocess/shlex best practice.
    """
    try:
        if host == "ai-legion":
            p = subprocess.run(["bash", "-s"], input=SYSFS_USB_SCRIPT,
                               capture_output=True, text=True, timeout=30)
        else:
            p = subprocess.run(
                ["ssh", "-o", "ConnectTimeout=5", f"root@{host}", "bash", "-s"],
                input=SYSFS_USB_SCRIPT, capture_output=True, text=True,
                timeout=40)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if p.returncode != 0 or not p.stdout:
        return None
    devs = []
    for line in p.stdout.splitlines():
        vidpid, _, rest = line.partition("|")
        serial, _, product = rest.partition("|")
        devs.append((vidpid.strip(), serial.strip(), product.strip()))
    return devs


def parse_exporter_identities(resources_text):
    """Exporter resource params -> set of identity strings to match USB."""
    idents = set()
    for m in re.finditer(r"'(?:match|port)':\s*'([^']+)'", resources_text):
        idents.add(m.group(1))
    for m in re.finditer(r"VID:PID ([0-9a-f]{4}:[0-9a-f]{4})", resources_text):
        idents.add(m.group(1))
    # udev identities live in match strings even when the value is split
    # across quoted python-continuations ("'... 6860 '"
    # "'ID_SERIAL_SHORT=RF8NB01ZYAH'") — grab them wherever they appear.
    for m in re.finditer(r"ID_SERIAL_SHORT=([0-9A-Za-z:-]+)", resources_text):
        idents.add(m.group(1))
    for m in re.finditer(r"STM32_STLink_([0-9A-Fa-f]{8,})", resources_text):
        idents.add(m.group(1))
    return idents


def usb_unclaimed(devices, exporter_idents):
    """Devices on a host no exporter identity claims and no allowlist covers.

    An exporter identity claims a device when its VID:PID matches an
    ID_VENDOR_ID/ID_MODEL_ID pair (or bare VID:PID string), or when the
    device's USB serial matches an ID_SERIAL_SHORT / by-id path fragment.
    """
    vidpids = set()
    for i in exporter_idents:
        m = re.search(r"ID_VENDOR_ID=([0-9a-f]{4})\s+ID_MODEL_ID=([0-9a-f]{4})", i)
        if m:
            vidpids.add(f"{m.group(1)}:{m.group(2)}")
        if re.fullmatch(r"[0-9a-f]{4}:[0-9a-f]{4}", i):
            vidpids.add(i)
    serials = {i for i in exporter_idents if len(i) >= 8
               and re.fullmatch(r"[0-9A-Za-z:-]+", i)
               and ":" not in i or "STM32_STLink_" in i}
    serials = {s.replace("STM32_STLink_", "") for s in serials if s}
    all_idents_lower = " ".join(exporter_idents).lower()
    flags = []
    for vidpid, serial, product in devices:
        vidpad = vidpid.split(":")[0] + ":"
        if any(vidpad.startswith(b) or vidpid.startswith(b) for b in BENIGN_USB_IDS):
            continue
        if vidpid in vidpids:
            continue
        if serial and serial in exporter_idents:
            continue
        # network-class claims (NetworkSmartcardReader et al.) carry no
        # USB match; a place whose show-text names the product claims it
        prod = product.lower().split()[0] if product else ""
        if prod and prod in all_idents_lower:
            continue
        flags.append(f"{vidpid} {product} (serial {serial or 'none'})")
    return flags


def resource_unregistered(resources_text, lab_yaml_text):
    """Exporter USB serial identities absent from lab.yaml (info-level)."""
    if not lab_yaml_text:
        return None  # registry unavailable -> skip check
    missing = []
    for m in re.finditer(r"ID_SERIAL_SHORT=([0-9A-Za-z:-]+)", resources_text):
        serial = m.group(1)
        if serial not in lab_yaml_text:
            missing.append(serial)
    return missing


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--gate", action="store_true",
                    help="exit 1 on FLAG (report mode always exits 0)")
    ap.add_argument("--hosts", default=",".join(DEFAULT_HOSTS))
    args = ap.parse_args()
    hosts = [h.strip() for h in args.hosts.split(",") if h.strip()]

    results = []          # (verdict, check, detail)
    who_text = labgrid(["who"])
    # `resources` prints only paths; identities (udev matches, by-id
    # ports, serials) live in per-place `show`. Aggregate all of them so
    # a device claimed by ANY place does not false-flag.
    resources_text = labgrid(["resources"]) or ""
    places_text = labgrid(["places"]) or ""
    for line in places_text.splitlines():
        name = line.strip().split()[0] if line.strip() else ""
        if not name or name.startswith("-") or name.lower().startswith("place"):
            continue
        show = labgrid(["-p", name, "show"])
        if show:
            resources_text += "\n" + show
    exporter_idents = parse_exporter_identities(resources_text)

    # 1. held-unannounced
    if who_text is None:
        results.append(("SKIP", "held-unannounced", "coordinator unreachable"))
    else:
        holds = parse_who(who_text)
        seats_recent = parse_seats_recent(SEATS_PATH)
        for user, host, place, changed in holds:
            if place in seats_recent:
                results.append(("PASS", "held-unannounced",
                                f"{place} held by {user}@{host} (announced)"))
            else:
                results.append(("FLAG", "held-unannounced",
                                f"{place} held by {user}@{host} since {changed} "
                                f"— NOT in recent SEATS.md sections"))
        if not holds:
            results.append(("PASS", "held-unannounced", "no places held"))

    # 2. usb-unclaimed
    for host in hosts:
        devices = parse_usb_devices(host)
        if devices is None:
            results.append(("SKIP", "usb-unclaimed", f"{host}: unreachable"))
            continue
        for line in usb_unclaimed(devices, exporter_idents):
            results.append(("FLAG", "usb-unclaimed", f"{host}: {line}"))
        if not usb_unclaimed(devices, exporter_idents):
            results.append(("PASS", "usb-unclaimed",
                            f"{host}: all non-benign devices claimed"))

    # 3. resource-unregistered (info)
    try:
        lab_yaml_text = open(LAB_YAML, encoding="utf-8", errors="replace").read()
    except OSError:
        lab_yaml_text = None
    miss = resource_unregistered(resources_text, lab_yaml_text)
    if miss is None:
        results.append(("SKIP", "resource-unregistered",
                        "lab.yaml unavailable"))
    elif miss:
        for serial in sorted(set(miss)):
            results.append(("INFO", "resource-unregistered",
                            f"exporter serial {serial} not in lab.yaml"))
    else:
        results.append(("PASS", "resource-unregistered",
                        "all exporter serials registered"))

    for verdict, check, detail in results:
        print(f"{verdict:5} {check:22} {detail}")
    if args.gate and any(v == "FLAG" for v, _, _ in results):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
