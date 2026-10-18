#!/usr/bin/env python3
"""Dump-and-retrieve workflow for esp32-ccid firmware (issue #66 direction).

Instead of live debugging over the CCID-shared wire, induce a snapshot and
analyze it offline: send the CCID Escape 0xD1 command (firmware acks, then
panics so the panic handler writes a flash coredump), wait for the reboot,
then read + decode the coredump partition with Espressif's espcoredump.

Usage:
    ./esp32_coredump.py trigger <port>            # send Escape 0xD1 over serial
    ./esp32_coredump.py retrieve <elf> [--port P] # read + decode the dump
        [--names]                                 # + raw task-name scan (no gdb)
    ./esp32_coredump.py erase <port> [--chip C]   # clear the coredump partition
                                                  # (--chip skips the prompt;
                                                  #  non-interactive shells MUST
                                                  #  pass it — issue #251)

Examples:
    # nucula snapshot round-trip (build first, adjust ELF path per board)
    ./esp32_coredump.py trigger /dev/ttyACM0
    sleep 6   # coredump write + reboot
    ./esp32_coredump.py retrieve \\
        /root/.cargo-target/riscv32imc-esp-espidf/debug/esp32-ccid

Coredump partition: 0x3F0000, 64 KB (partitions-ota.csv). Requires gdb-multiarch
and a python with the `esp_coredump` module (issue #251 discovery chain: the
esptool-driven esp-idf python env under the cargo target dir, the bench-shared
IDF tree at /root/esp/esp-idf*, or a venv carrying the module — override with
ESP_COREDUMP_IDF_PY / ESP_COREDUMP_PYTHON). `--names` needs neither: it scans
the raw dump with the stdlib only.
"""

import argparse
import glob
import os
import subprocess
import sys
import time

COREDUMP_OFFSET = 0x3F0000
COREDUMP_SIZE = 0x10000

SYNC, CTRL_ACK = 0x03, 0x06
PC_TO_RDR_ESCAPE = 0x6B


def gempc_frame(payload: bytes, seq: int = 0) -> bytes:
    ccid = bytes([PC_TO_RDR_ESCAPE]) + len(payload).to_bytes(4, "little") + bytes(
        [0, seq, 0, 0, 0]
    ) + payload
    frame = bytes([SYNC, CTRL_ACK]) + ccid
    lrc = 0
    for b in frame:
        lrc ^= b
    return frame + bytes([lrc])


def trigger(port: str) -> int:
    import serial

    with serial.Serial(port, 115200, timeout=0.2) as s:
        # Wait for boot/banner traffic to stop: right after a flash or
        # reset the board is still printing and a frame sent now is
        # processed (or dropped) unpredictably mid-boot.
        quiet_deadline = time.time() + 15
        while time.time() < quiet_deadline:
            if not s.read(4096):
                break
        s.reset_input_buffer()
        s.write(gempc_frame(b"\xD1"))
        # Chatty ports (netlog on UART0) bury the ack in boot/LED noise —
        # read until the echo+ack pair shows up or ~3 s pass.
        wire = b""
        deadline = time.time() + 3
        while time.time() < deadline:
            wire += s.read(4096)
            if bytes([0x03, 0x06, 0x83]) in wire and b"\xD1" in wire:
                break
        print(f"received {len(wire)} bytes: {wire[:120].hex()}...")
        if bytes([0x03, 0x06, 0x83]) not in wire:
            print("warning: no RDR_to_PC_Escape ack seen — frame may not have "
                  "been parsed before the panic", file=sys.stderr)
    print("snapshot requested; firmware reboots after the coredump write "
          "(watch the console); run `retrieve` next")
    return 0


def find_espcoredump_py() -> str:
    override = os.environ.get("ESP_COREDUMP_IDF_PY")
    if override:
        return override
    target_dir = os.environ.get("CARGO_TARGET_DIR", os.path.expanduser("~/.cargo-target"))
    candidates = [
        sorted(glob.glob(f"{target_dir}/.embuild/espressif/esp-idf/v*/"
                         "components/espcoredump/espcoredump.py")),
        sorted(glob.glob(os.path.expanduser(
            "~/esp/esp-idf*/components/espcoredump/espcoredump.py"))),
    ]
    for hits in candidates:
        if hits:
            return hits[-1]
    sys.exit("no espcoredump.py found (tried $CARGO_TARGET_DIR/.embuild and "
             "~/esp/esp-idf*) — set ESP_COREDUMP_IDF_PY")


def _python_has_esp_coredump(python: str) -> bool:
    if not os.path.exists(python):
        return False
    return subprocess.run(
        [python, "-c", "import esp_coredump"],
        capture_output=True,
    ).returncode == 0


def find_idf_python() -> str:
    override = os.environ.get("ESP_COREDUMP_PYTHON")
    if override:
        return override
    target_dir = os.environ.get("CARGO_TARGET_DIR", os.path.expanduser("~/.cargo-target"))
    hits = sorted(glob.glob(f"{target_dir}/.embuild/espressif/python_env/idf*_py*_env/bin/python"))
    if hits:
        return hits[-1]
    script_dir = os.path.dirname(os.path.abspath(__file__))
    repo_root = os.path.dirname(os.path.dirname(script_dir))
    for candidate in (sys.executable,
                      f"{repo_root}/.omo/esp32-hil/venv/bin/python"):
        if _python_has_esp_coredump(candidate):
            return candidate
    sys.exit("no python with the esp_coredump module (PEP-668 blocks system "
             "pip) — create one: python3 -m venv <repo>/.omo/esp32-hil/venv "
             "&& <venv>/bin/pip install esp-coredump, or set "
             "ESP_COREDUMP_PYTHON")


def esptool(chip: str, port: str, *args: str) -> subprocess.CompletedProcess:
    cmd = ["esptool", "--chip", chip, "-p", port, *args]
    print("+", " ".join(cmd))
    return subprocess.run(cmd, check=True)


def guess_chip(elf: str) -> str:
    return "esp32c3" if "riscv32imc" in elf else "esp32"


def scan_task_names(raw: bytes) -> None:
    runs: dict[str, int] = {}
    run = b""
    for b in raw + b"\x00":
        if 32 <= b < 127:
            run += bytes([b])
            continue
        if 4 <= len(run) <= 24:
            runs[run] = runs.get(run, 0) + 1
        run = b""
    print("--- raw task-name scan (gdb-free; FreeRTOS TCB pcTaskName strings")
    print("--- and the FWID git rev baked into ESP_EXTRA_INFO)")
    for run_bytes, count in sorted(runs.items(), key=lambda kv: -kv[1]):
        text = run_bytes.decode()
        core = text.lstrip("?@!#$%&*+-=:;<>,.^_~ |\\/\"'")
        is_task = core.split("\x00")[0] in {
            "main", "IDLE0", "IDLE1", "ipc0", "ipc1", "esp_timer"}
        is_thread = core.startswith("pthread") or core.startswith("esp32")
        is_rev = len(core) in (7, 8) and all(c in "0123456789abcdef" for c in core)
        if is_task or is_thread or is_rev or count > 1:
            kind = ("task" if is_task else
                    "std::thread task" if is_thread else
                    "git-rev candidate" if is_rev else
                    "repeated")
            print(f"  {count:2d}x {text!r:26} {kind}")


def retrieve(elf: str, port: str | None, names: bool) -> int:
    if not os.path.exists(elf):
        sys.exit(f"ELF not found: {elf}")
    chip = guess_chip(elf)
    raw = "/tmp/opencode/esp32-ccid-coredump.bin"
    if port:
        esptool(chip, port, "read-flash", str(COREDUMP_OFFSET), str(COREDUMP_SIZE), raw)
    elif not os.path.exists(raw):
        sys.exit("no --port and no cached dump at " + raw)
    if names:
        with open(raw, "rb") as f:
            scan_task_names(f.read())
    out = subprocess.run(
        [find_idf_python(), find_espcoredump_py(),
         "--chip", chip, "info_corefile", "--core", raw, "--core-format", "raw",
         "--gdb", "gdb-multiarch", elf],
        check=False,
    )
    print("\nto re-decode later: esp32_coredump.py retrieve <elf>   "
          "(cached raw dump: " + raw + ")")
    print("clear it when done:                   "
          "esp32_coredump.py erase <port>")
    return out.returncode


def guess_chip_from_port(port: str) -> str | None:
    if "Espressif_USB_JTAG" in port or "ttyACM" in port:
        return "esp32c3"
    if "Hades2001" in port or "ttyUSB" in port:
        return "esp32"
    return None


def erase(port: str, chip: str | None) -> int:
    if not chip:
        chip = guess_chip_from_port(port)
    if not chip:
        if not sys.stdin.isatty():
            sys.exit("cannot guess chip from the port and stdin is not a "
                     "terminal — pass --chip esp32|esp32c3 (issue #251)")
        chip = input("chip (esp32c3/esp32): ").strip() or "esp32c3"
    esptool(chip, port, "erase-region", str(COREDUMP_OFFSET), str(COREDUMP_SIZE))
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("trigger", help="send Escape 0xD1 over the CCID serial wire")
    t.add_argument("port")
    r = sub.add_parser("retrieve", help="read + decode the coredump partition")
    r.add_argument("elf", help="firmware ELF matching the running build")
    r.add_argument("--port", help="serial port (skips the read if omitted)")
    r.add_argument("--names", action="store_true",
                   help="also scan raw task names / baked rev (stdlib only)")
    e = sub.add_parser("erase", help="erase the coredump partition")
    e.add_argument("port")
    e.add_argument("--chip", choices=["esp32", "esp32c3"],
                   help="esptool chip (else guessed from the port path; "
                        "required in non-interactive shells)")
    args = ap.parse_args()

    if args.cmd == "trigger":
        return trigger(args.port)
    if args.cmd == "retrieve":
        return retrieve(args.elf, args.port, args.names)
    return erase(args.port, args.chip)


if __name__ == "__main__":
    sys.exit(main())
