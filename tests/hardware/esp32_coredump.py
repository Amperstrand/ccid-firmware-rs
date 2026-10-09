#!/usr/bin/env python3
"""Dump-and-retrieve workflow for esp32-ccid firmware (issue #66 direction).

Instead of live debugging over the CCID-shared wire, induce a snapshot and
analyze it offline: send the CCID Escape 0xD1 command (firmware acks, then
panics so the panic handler writes a flash coredump), wait for the reboot,
then read + decode the coredump partition with Espressif's espcoredump.

Usage:
    ./esp32_coredump.py trigger <port>            # send Escape 0xD1 over serial
    ./esp32_coredump.py retrieve <elf> [--port P] # read + decode the dump
    ./esp32_coredump.py erase <port>              # clear the coredump partition

Examples:
    # nucula snapshot round-trip (build first, adjust ELF path per board)
    ./esp32_coredump.py trigger /dev/ttyACM0
    sleep 6   # coredump write + reboot
    ./esp32_coredump.py retrieve \\
        /root/.cargo-target/riscv32imc-esp-espidf/debug/esp32-ccid

Coredump partition: 0x3F0000, 64 KB (partitions-ota.csv). Requires gdb-multiarch
and the esptool-driven esp-idf python env (auto-discovered under the cargo
target dir; override with ESP_COREDUMP_IDF_PY).
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
    hits = sorted(glob.glob(f"{target_dir}/.embuild/espressif/esp-idf/v*/"
                            "components/espcoredump/espcoredump.py"))
    if not hits:
        sys.exit(f"no espcoredump.py under {target_dir}/.embuild — set ESP_COREDUMP_IDF_PY")
    return hits[-1]


def find_idf_python() -> str:
    override = os.environ.get("ESP_COREDUMP_PYTHON")
    if override:
        return override
    target_dir = os.environ.get("CARGO_TARGET_DIR", os.path.expanduser("~/.cargo-target"))
    hits = sorted(glob.glob(f"{target_dir}/.embuild/espressif/python_env/idf*_py*_env/bin/python"))
    if hits:
        return hits[-1]
    return sys.executable


def esptool(chip: str, port: str, *args: str) -> subprocess.CompletedProcess:
    cmd = ["esptool", "--chip", chip, "-p", port, *args]
    print("+", " ".join(cmd))
    return subprocess.run(cmd, check=True)


def guess_chip(elf: str) -> str:
    return "esp32c3" if "riscv32imc" in elf else "esp32"


def retrieve(elf: str, port: str | None) -> int:
    if not os.path.exists(elf):
        sys.exit(f"ELF not found: {elf}")
    chip = guess_chip(elf)
    raw = "/tmp/opencode/esp32-ccid-coredump.bin"
    if port:
        esptool(chip, port, "read-flash", str(COREDUMP_OFFSET), str(COREDUMP_SIZE), raw)
    elif not os.path.exists(raw):
        sys.exit("no --port and no cached dump at " + raw)
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


def erase(port: str) -> int:
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
    e = sub.add_parser("erase", help="erase the coredump partition")
    e.add_argument("port")
    args = ap.parse_args()

    if args.cmd == "trigger":
        return trigger(args.port)
    if args.cmd == "retrieve":
        return retrieve(args.elf, args.port)
    return erase(args.port)


if __name__ == "__main__":
    sys.exit(main())
