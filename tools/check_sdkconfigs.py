#!/usr/bin/env python3
"""Gate every shipped esp32-ccid sdkconfig against CCID-wire console contamination.

Issue #78 (original gate): the M5Stick bench config (sdkconfig-xtensa.full)
enabled the ESP-IDF console on UART0 — the same wire the GemPC Twin CCID
protocol serves. Boot/application log traffic corrupts the binary protocol
channel (libccidtwin's parser has no resync — clean-wire doctrine, AGENTS.md).

Rules (per config family — a config MUST be classified deliberately;
unclassified configs fail the gate, there are no silent opt-outs):

  - UART0-CCID boards (classic m5stick/m5atom serial readers):
      console MUST be disabled (CONFIG_ESP_CONSOLE_NONE=y, no UART choice).
  - C3 CDC boards (nucula — CCID over the ROM USB-Serial/JTAG CDC):
      console MUST be CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG=y (the FWID banner
      surface) with no UART console. Firmware sets the log ceiling to Off
      right after the boot banner (#131 clean-wire).
  - EXEMPT boards (CCID rides a non-UART transport; UART0 console is the
      intended debug/diagnostic surface):
      - sdkconfig-xtensa-spp.full    — CCID over BT SPP (UART0 free)
      - sdkconfig-xtensa-vpcd.full   — CCID over WiFi/TCP vpcd (UART0 free)
      - sdkconfig-xtensa-ble.full    — CCID on UART0, logs to the BLE ring;
        early ROM/bootloader output still hits UART0 and the serve loop
        purges it (documented in AGENTS.md BLE section) — tolerated, but
        tracked here so it can never silently spread to clean boards.
    Exempt configs are still required to pin their console choice EXPLICITLY
    (a missing/implicit console is a drift smell).

Exit 0 = all configs compliant; 1 = at least one violation.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CFG_DIR = ROOT / "firmware" / "esp32-ccid"

# Boards whose CCID protocol rides UART0 — console there is corruption.
UART0_CCID = {
    "sdkconfig.defaults.esp32",
    "sdkconfig-xtensa.full",
}
# C3 boards: CCID + console share the USB-Serial/JTAG CDC; firmware gates
# logging after the boot banner (clean-wire #131).
# sdkconfig-c3-ble-diag.full is the #165 DIAGNOSTIC exception: same CDC
# console pinning, but BT stack logs deliberately UNSILENCED at DEBUG —
# it exists only for on-device capture via build.sh c3-ccid-ble-diag and
# must never be flashed as a serving image.
C3_CDC = {
    "sdkconfig.defaults.esp32c3",
    "sdkconfig.full",
    "sdkconfig.wallet-test",
    "sdkconfig-c3-ble.full",
    "sdkconfig-c3-ble-diag.full",
}
# CCID on another transport; UART0 console is intended. See module docstring.
EXEMPT = {
    "sdkconfig-xtensa-spp.full": "CCID over BT SPP",
    "sdkconfig-xtensa-vpcd.full": "CCID over WiFi/TCP (vpcd)",
    "sdkconfig-xtensa-ble.full": "BLE log ring; ROM output purged by serve loop",
}
# Convenience defaults with no console of their own (family-neutral).
NO_CONSOLE_LINES = {
    "sdkconfig.defaults",
}

# Canonical console-choice symbols only. The legacy no-prefix aliases
# (CONFIG_CONSOLE_UART_DEFAULT etc.) are inert residues that kconfig
# migration keeps re-emitting in the normalized .full files — they do
# not select anything on IDF 5.x and are deliberately ignored here.
UART_CONSOLE_CHOICES = (
    "CONFIG_ESP_CONSOLE_UART_DEFAULT",
    "CONFIG_ESP_CONSOLE_UART_CUSTOM",
    "CONFIG_ESP_CONSOLE_UART_NONE",
)


def sets_y(text: str, key: str) -> bool:
    return re.search(rf"^{re.escape(key)}=y$", text, re.M) is not None


def sets_any(text: str, keys) -> str | None:
    for key in keys:
        if sets_y(text, key):
            return key
    return None


# Issue #446: the xtensa .full files carried a dead absolute
# CONFIG_PARTITION_TABLE_CUSTOM_FILENAME (an old bench box's
# /home/ubuntu/... path) SHADOWED by a later bare-name assignment —
# inert in kconfig's last-wins processing, but a landmine for any tool
# (or reader) that takes the first occurrence, and pure confusion for
# humans. Two universal rules lock the class:
#  - the filename must be BARE (no path separators — the file is
#    delivered into the embuild project dir by the ESP_IDF_GLOB_PARTCSV_*
#    injection; see .cargo/config.toml and build.sh);
#  - partition-table keys are assigned at most once (the duplicate WAS
#    the hiding place).
PARTITION_KEYS = (
    "CONFIG_PARTITION_TABLE_CUSTOM_FILENAME",
    "CONFIG_PARTITION_TABLE_FILENAME",
)


def check_partition_table(name: str, text: str) -> list[str]:
    errs = []
    for key in PARTITION_KEYS:
        assignments = re.findall(rf'^{key}="([^"]*)"$', text, re.M)
        if len(assignments) > 1:
            errs.append(
                f"{name}: {key} assigned {len(assignments)}x "
                f"({assignments}) — exactly one assignment (the "
                "duplicate-shadowing class of #446)"
            )
        for value in assignments:
            if "/" in value or value != Path(value).name:
                errs.append(
                    f"{name}: {key} must be a BARE csv filename, got "
                    f"{value!r} (#446: the csv arrives via the "
                    "ESP_IDF_GLOB_PARTCSV injection, not via a path)"
                )
    return errs


def check_config(name: str) -> list[str]:
    path = CFG_DIR / name
    if not path.is_file():
        return [f"{name}: missing (stale gate entry — update tools/check_sdkconfigs.py)"]
    text = path.read_text()
    errs = []

    errs.extend(check_partition_table(name, text))

    uart_choice = sets_any(text, UART_CONSOLE_CHOICES)

    if name in UART0_CCID:
        if not sets_y(text, "CONFIG_ESP_CONSOLE_NONE"):
            errs.append(f"{name}: UART0-CCID board must set CONFIG_ESP_CONSOLE_NONE=y")
        if uart_choice:
            errs.append(f"{name}: UART0-CCID board must not select a UART console ({uart_choice}=y)")
    elif name in C3_CDC:
        if not sets_y(text, "CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG"):
            errs.append(f"{name}: C3 CDC board must set CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG=y")
        if uart_choice:
            errs.append(f"{name}: C3 CDC board must not select a UART console ({uart_choice}=y)")
    elif name in EXEMPT:
        # Exempt boards must still pin their console choice explicitly.
        if not sets_y(text, "CONFIG_ESP_CONSOLE_UART_DEFAULT") and not sets_y(
            text, "CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG"
        ):
            errs.append(
                f"{name}: exempt board ({EXEMPT[name]}) must pin an explicit "
                "console choice (UART_DEFAULT or USB_SERIAL_JTAG)"
            )
    elif name in NO_CONSOLE_LINES:
        pass
    else:
        errs.append(
            f"{name}: unclassified sdkconfig — add it to UART0_CCID / C3_CDC / "
            "EXEMPT (with justification) or NO_CONSOLE_LINES in tools/check_sdkconfigs.py"
        )
    return errs


def main() -> int:
    if not CFG_DIR.is_dir():
        print(f"check-sdconfigs: FAIL: missing {CFG_DIR}", file=sys.stderr)
        return 1

    shipped = {p.name for p in CFG_DIR.glob("sdkconfig*")}
    classified = UART0_CCID | C3_CDC | set(EXEMPT) | NO_CONSOLE_LINES
    failures: list[str] = []

    for name in sorted(shipped):
        failures.extend(check_config(name))

    missing = sorted(classified - shipped)
    for name in missing:
        failures.append(f"{name}: gate entry has no such file (drift — prune the gate)")

    for f in failures:
        print(f"check-sdkconfigs: FAIL: {f}", file=sys.stderr)
    n = len(shipped)
    print(f"check-sdkconfigs: {n} configs checked, {len(failures)} failures")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
