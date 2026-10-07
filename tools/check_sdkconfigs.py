#!/usr/bin/env python3
"""Gate every shipped esp32-ccid sdkconfig against UART0 console contamination.

Issue #78: the M5Stick bench config (sdkconfig-xtensa.full) enabled the
ESP-IDF console on UART0 — the same wire the GemPC Twin CCID protocol
serves. Boot/application log traffic then corrupts the binary protocol
channel. The per-config defaults were already clean; the .full bench
config was not, and nothing validated it.

Rules (per config family):
  - xtensa (ESP32, CCID on UART0 via GPIO1/3):
      CONFIG_ESP_CONSOLE_NONE=y and no active console-UART choice
  - riscv32/c3 (nucula, CCID over the ROM USB-Serial/JTAG CDC):
      CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG=y and no console-UART choice

Any NEW sdkconfig* file must be added to a family below deliberately —
unclassified configs fail the gate (no silent opt-outs).

Exit 0 = all configs clean; 1 = at least one violation.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CFG_DIR = ROOT / "firmware" / "esp32-ccid"

XTENSA = {
    "sdkconfig.defaults.esp32",
    "sdkconfig-xtensa.full",
}
C3 = {
    "sdkconfig.defaults.esp32c3",
    "sdkconfig.full",
}
NO_CONSOLE_LINES = {
    "sdkconfig.defaults",
    "sdkconfig.defaults.ble",
}

UART_CONSOLE_CHOICES = (
    "CONFIG_ESP_CONSOLE_UART_DEFAULT",
    "CONFIG_ESP_CONSOLE_UART_CUSTOM",
)


def sets_y(text: str, key: str) -> bool:
    return re.search(rf"^{re.escape(key)}=y$", text, re.M) is not None


def check_config(path: Path, console_none: bool) -> list[str]:
    text = path.read_text()
    violations = []
    if console_none:
        if not sets_y(text, "CONFIG_ESP_CONSOLE_NONE"):
            violations.append(
                f"{path.name}: CONFIG_ESP_CONSOLE_NONE=y required — UART0 is the "
                "CCID protocol line (issue #78)"
            )
    else:
        if not sets_y(text, "CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG"):
            violations.append(
                f"{path.name}: CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG=y required "
                "(nucula console; a UART console would fight the transport)"
            )
    for key in UART_CONSOLE_CHOICES:
        if sets_y(text, key):
            violations.append(f"{path.name}: {key}=y — console on a UART is forbidden")
    return violations


def main() -> int:
    known = XTENSA | C3 | NO_CONSOLE_LINES
    on_disk = {p.name for p in CFG_DIR.glob("sdkconfig*")}
    failures = []

    unclassified = sorted(on_disk - known)
    for name in unclassified:
        failures.append(
            f"{name}: new sdkconfig not classified in {Path(__file__).name} "
            "(add to XTENSA/C3/NO_CONSOLE_LINES — deliberate choice required)"
        )

    for name in sorted(XTENSA & on_disk):
        failures += check_config(CFG_DIR / name, console_none=True)
    for name in sorted(C3 & on_disk):
        failures += check_config(CFG_DIR / name, console_none=False)

    for name in sorted((XTENSA | C3) - on_disk):
        failures.append(f"{name}: classified config missing from {CFG_DIR}")

    for line in failures:
        print(f"sdkconfig-gate: FAIL: {line}")
    if not failures:
        print(
            f"sdkconfig-gate: OK — {len(on_disk)} config(s) clean "
            f"(xtensa console=NONE, c3 console=USB_SERIAL_JTAG)"
        )
        return 0
    print(
        "sdkconfig-gate: FAILURES — UART0 console output must never share the "
        "CCID serial line (issue #78)",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
