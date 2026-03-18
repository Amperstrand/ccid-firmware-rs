#!/usr/bin/env python3
"""Shared library for CCID soak tests.

Provides:
- Reader discovery and identification
- pcscd lifecycle management with CCID capture
- CCID message parsing from pcscd COMM logs
- Behavioral comparison between Gemalto and firmware readers
- GitHub issue filing for bugs found
- Structured result logging
"""

import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

import smartcard.System
from smartcard.scard import SCARD_PROTOCOL_T1
from smartcard.CardRequest import CardRequest
from smartcard.Exceptions import NoCardException, CardConnectionException

GEMALTO_SERIAL = "BCF852F0"
FIRMWARE_SERIAL = "CT30-001"
LOG_BASE = Path("/home/ubuntu/gt/ccid_firmware/soak-test-logs")
RIG_DIR = Path("/home/ubuntu/gt/ccid_firmware/mayor/rig")
GITHUB_REPO = "Amperstrand/ccid-firmware-rs"

CCID_MSG_TYPES = {
    0x61: "RDR_to_PC_DataBlock",
    0x62: "RDR_to_PC_SlotStatus",
    0x63: "RDR_to_PC_Parameters",
    0x64: "RDR_to_PC_Escape",
    0x65: "RDR_to_PC_Clock",
    0x66: "RDR_to_PC_DataRate",
    0x81: "PC_to_RDR_IccPowerOn",
    0x82: "PC_to_RDR_IccPowerOff",
    0x83: "PC_to_RDR_GetSlotStatus",
    0x84: "PC_to_RDR_GetParameters",
    0x85: "PC_to_RDR_ResetParameters",
    0x86: "PC_to_RDR_SetParameters",
    0x87: "PC_to_RDR_XfrBlock",
    0x88: "PC_to_RDR_GetKey",
    0x89: "PC_to_RDR_Secure",
    0x8A: "PC_to_RDR_T0APDU",
    0x8B: "PC_to_RDR_Mechanical",
    0x8C: "PC_to_RDR_Abort",
    0x8D: "PC_to_RDR_SetDataRateAndClockFrequency",
    0x6B: "PC_to_RDR_Escape",
}

CMD_STATUS_MAP = {
    0x00: "CMD_OK",
    0x01: "CMD_FAILED",
    0x02: "CMD_TIME_EXTENSION",
}

ICC_STATUS_MAP = {
    0x00: "ICC_PRESENT_AND_ACTIVE",
    0x01: "ICC_PRESENT_INACTIVE",
    0x02: "ICC_NOT_PRESENT",
    0x03: "ICC_NO_STATUS_CHANGE",
    0x04: "ICC_MUTE",
}


@dataclass
class CcidMessage:
    direction: str
    msg_type: int
    msg_type_name: str
    data_len: int
    slot: int
    seq: int
    b_status: int = 0
    b_error: int = 0
    b_clock_status: int = 0
    data: bytes = b""
    timestamp_us: Optional[int] = None

    @property
    def cmd_status(self) -> str:
        return CMD_STATUS_MAP.get(self.b_status & 0xC0, f"UNKNOWN(0x{(self.b_status & 0xC0):02X})")

    @property
    def icc_status(self) -> str:
        return ICC_STATUS_MAP.get(self.b_status & 0x03, f"UNKNOWN(0x{(self.b_status & 0x03):02X})")

    @property
    def error_name(self) -> str:
        if self.b_error == 0x00:
            return "NONE"
        errors = {
            0x01: "CMD_ABORTED", 0x02: "ICC_MUTE", 0x03: "XFR_PARITY_ERROR",
            0x04: "XFR_OVERRUN", 0x05: "HW_ERROR", 0x06: "BAD_ATR",
            0x07: "ICC_PROTOCOL_NOT_SUPPORTED", 0x09: "PIN_CANCELLED",
            0x0A: "CMD_NOT_SUPPORTED", 0x0B: "PIN_TIMEOUT",
            0x0C: "PIN_DIFFERENT", 0x0D: "CARD_NOT_POWERED",
            0x0E: "REMOVED_CARD", 0x0F: "INSERTED_WRONG_CARD",
            0xFE: "CMD_SLOT_BUSY",
        }
        return errors.get(self.b_error, f"UNKNOWN(0x{self.b_error:02X})")

    def to_dict(self) -> dict:
        d = asdict(self)
        d["data"] = self.data.hex()
        d["cmd_status"] = self.cmd_status
        d["icc_status"] = self.icc_status
        d["error_name"] = self.error_name
        return d


@dataclass
class TestResult:
    suite_name: str
    test_name: str
    passed: bool
    gemalto_ok: bool = True
    firmware_ok: bool = True
    gemalto_error: str = ""
    firmware_error: str = ""
    gemalto_response: str = ""
    firmware_response: str = ""
    difference: str = ""
    bug_filed: bool = False
    github_issue_url: str = ""
    notes: str = ""
    timestamp: str = field(default_factory=lambda: datetime.now(timezone.utc).isoformat())

    def to_dict(self) -> dict:
        return asdict(self)


@dataclass
class SuiteResult:
    suite_name: str
    suite_num: int
    started_at: str = field(default_factory=lambda: datetime.now(timezone.utc).isoformat())
    completed_at: str = ""
    total_tests: int = 0
    passed: int = 0
    failed: int = 0
    skipped: int = 0
    bugs_found: int = 0
    results: list = field(default_factory=list)
    notes: str = ""

    def to_dict(self) -> dict:
        return asdict(self)


def discover_readers(timeout: int = 30) -> dict:
    readers = {}
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            all_readers = smartcard.System.readers()
            for r in all_readers:
                name = str(r)
                if GEMALTO_SERIAL in name:
                    readers["gemalto"] = {"reader": r, "name": name, "serial": GEMALTO_SERIAL}
                elif FIRMWARE_SERIAL in name:
                    readers["firmware"] = {"reader": r, "name": name, "serial": FIRMWARE_SERIAL}
            if "gemalto" in readers and "firmware" in readers:
                return readers
        except Exception:
            pass
        time.sleep(1)
    return readers


def connect_reader(reader_info: dict, protocol=None):
    if protocol is None:
        protocol = SCARD_PROTOCOL_T1
    for attempt in range(3):
        try:
            conn = reader_info["reader"].createConnection()
            conn.connect(protocol)
            return conn
        except Exception as e:
            if attempt == 2:
                raise
            time.sleep(1)
    raise CardConnectionException(f"Failed to connect after 3 attempts")


def get_atr(conn):
    atr = conn.getATR()
    if isinstance(atr, list):
        return bytes(atr)
    return atr


def disconnect_reader(conn):
    try:
        conn.disconnect()
    except Exception:
        pass


def restart_pcscd(debug_level: int = 0x0007, log_path: Optional[str] = None):
    subprocess.run(["sudo", "pkill", "pcscd"], capture_output=True)
    time.sleep(1)
    subprocess.run(["sudo", "rm", "-f", "/run/pcscd/pcscd.comm"], capture_output=True)
    time.sleep(0.5)

    env = os.environ.copy()
    if debug_level:
        env["LIBCCID_DEBUG_LEVEL"] = str(debug_level)

    cmd = ["sudo", "pcscd", "-f", "-a"]
    if log_path:
        log_file = open(log_path, "w", buffering=1)
        proc = subprocess.Popen(cmd, env=env, stdout=log_file, stderr=log_file)
        proc._log_file = log_file
    else:
        proc = subprocess.Popen(cmd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        proc._log_file = None
    _pcscd_procs.append(proc)

    time.sleep(2)
    return proc


_pcscd_procs = []

def stop_pcscd():
    subprocess.run(["sudo", "pkill", "pcscd"], capture_output=True)
    time.sleep(1)
    subprocess.run(["sudo", "rm", "-f", "/run/pcscd/pcscd.comm"], capture_output=True)
    for proc in list(_pcscd_procs):
        try:
            if hasattr(proc, '_log_file') and proc._log_file:
                proc._log_file.flush()
                proc._log_file.close()
        except Exception:
            pass
    _pcscd_procs.clear()


def transmit_apdu(conn, apdu: bytes, expect_sw: bool = True) -> tuple:
    try:
        data, sw1, sw2 = conn.transmit(list(apdu))
        if expect_sw:
            return bytes(data), (sw1 << 8) | sw2
        return bytes(data), None
    except CardConnectionException as e:
        return None, str(e)


def transmit_apdu_str(conn, apdu_hex: str) -> tuple:
    return transmit_apdu(conn, bytes.fromhex(apdu_hex))


def parse_ccid_log(log_path: str, device_serial: str) -> list:
    messages = []
    serial_pattern = re.escape(device_serial[:8]) if len(device_serial) >= 8 else re.escape(device_serial)
    pattern = re.compile(
        rf'(usb:\d+/\d+:libudev:\d+:[^ ]*\({serial_pattern}[^\)]*\))'
    )

    current_ts = 0
    with open(log_path, "r") as f:
        for line in f:
            ts_match = re.match(r'^(\d+)', line)
            if ts_match:
                current_ts = int(ts_match.group(1))

            device_match = pattern.search(line)
            if not device_match:
                continue

            dir_match = re.search(r'(->|<-)\s+([0-9A-Fa-f]{2}(?:\s+[0-9A-Fa-f]{2})*)', line)
            if not dir_match:
                continue

            direction = "->" if dir_match.group(1) == "->" else "<-"
            hex_bytes = bytes.fromhex(dir_match.group(2).replace(" ", ""))

            if len(hex_bytes) < 10:
                continue

            msg_type = hex_bytes[0]
            data_len = int.from_bytes(hex_bytes[1:5], "little")
            slot = hex_bytes[5]
            seq = hex_bytes[6]
            b_status = hex_bytes[7]
            b_error = hex_bytes[8]
            b_clock = hex_bytes[9]
            data = hex_bytes[10:]

            if len(data) < data_len:
                data = data + b"\x00" * (data_len - len(data))

            msg_name = CCID_MSG_TYPES.get(msg_type, f"UNKNOWN(0x{msg_type:02X})")

            msg = CcidMessage(
                direction="OUT" if direction == "->" else "IN",
                msg_type=msg_type,
                msg_type_name=msg_name,
                data_len=data_len,
                slot=slot,
                seq=seq,
                b_status=b_status,
                b_error=b_error,
                b_clock_status=b_clock,
                data=data[:data_len] if data_len > 0 else b"",
                timestamp_us=current_ts,
            )
            messages.append(msg)

    return messages


def compare_ccid_sequences(gemalto_msgs: list, firmware_msgs: list) -> list:
    diffs = []
    max_len = max(len(gemalto_msgs), len(firmware_msgs))

    for i in range(max_len):
        g = gemalto_msgs[i] if i < len(gemalto_msgs) else None
        f = firmware_msgs[i] if i < len(firmware_msgs) else None

        if g is None:
            diffs.append({
                "index": i,
                "type": "extra_in_firmware",
                "firmware": f.to_dict() if f else None,
            })
            continue
        if f is None:
            diffs.append({
                "index": i,
                "type": "extra_in_gemalto",
                "gemalto": g.to_dict() if g else None,
            })
            continue

        diff = {"index": i, "type": "field_diff", "fields": []}

        if g.msg_type != f.msg_type:
            diff["fields"].append({
                "field": "msg_type",
                "gemalto": f"{g.msg_type_name} (0x{g.msg_type:02X})",
                "firmware": f"{f.msg_type_name} (0x{f.msg_type:02X})",
            })

        if g.b_error != f.b_error and not _error_equivalent(g.b_error, f.b_error):
            diff["fields"].append({
                "field": "b_error",
                "gemalto": f"{g.error_name} (0x{g.b_error:02X})",
                "firmware": f"{f.error_name} (0x{f.b_error:02X})",
            })

        g_icc = g.b_status & 0x03
        f_icc = f.b_status & 0x03
        if g_icc != f_icc:
            diff["fields"].append({
                "field": "icc_status",
                "gemalto": g.icc_status,
                "firmware": f.icc_status,
            })

        g_cmd = g.b_status & 0xC0
        f_cmd = f.b_status & 0xC0
        if g_cmd != f_cmd:
            diff["fields"].append({
                "field": "cmd_status",
                "gemalto": g.cmd_status,
                "firmware": f.cmd_status,
            })

        if g.data != f.data and g.msg_type in (0x61, 0x62, 0x63, 0x64):
            diff["fields"].append({
                "field": "data",
                "gemalto": g.data[:64].hex() + ("..." if len(g.data) > 64 else ""),
                "firmware": f.data[:64].hex() + ("..." if len(f.data) > 64 else ""),
            })

        if diff["fields"]:
            diffs.append(diff)

    return diffs


def _error_equivalent(e1: int, e2: int) -> bool:
    if e1 == e2:
        return True
    known_equivalent = {(0x00, 0x00)}
    return (e1, e2) in known_equivalent or (e2, e1) in known_equivalent


def filter_same_direction(msgs: list, direction: str) -> list:
    return [m for m in msgs if m.direction == direction]


def filter_by_msg_type(msgs: list, msg_type: int) -> list:
    return [m for m in msgs if m.msg_type == msg_type]


def find_atr(msgs: list) -> Optional[bytes]:
    for m in msgs:
        if m.msg_type == 0x62 and m.direction == "IN" and len(m.data) > 0:
            return m.data
    return None


def find_escape_response(msgs: list) -> Optional[CcidMessage]:
    for m in msgs:
        if m.msg_type == 0x64 and m.direction == "IN":
            return m
    return None


def file_github_issue(title: str, body: str, labels: list = None) -> Optional[str]:
    gh_token = os.environ.get("GH_TOKEN", "")
    if not gh_token:
        hosts_path = Path.home() / ".config" / "gh" / "hosts.yml"
        if hosts_path.exists():
            import yaml
            try:
                cfg = yaml.safe_load(hosts_path.read_text())
                gh_token = cfg.get("github.com", {}).get("users", {}).get("Amperstrand", {}).get("oauth_token", "")
            except Exception:
                pass
        if not gh_token:
            hosts_path2 = Path("/home/ubuntu/.config/gh/hosts.yml")
            if hosts_path2.exists():
                import re
                try:
                    content = hosts_path2.read_text()
                    m = re.search(r'oauth_token:\s*(\S+)', content)
                    if m:
                        gh_token = m.group(1)
                except Exception:
                    pass

    env = os.environ.copy()
    if gh_token:
        env["GH_TOKEN"] = gh_token
        env["GH_AUTH_TOKEN"] = gh_token

    cmd = ["gh", "issue", "create", "--repo", GITHUB_REPO, "--title", title, "--body", body]
    if labels:
        cmd.extend(["--label", ",".join(labels)])
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=30, env=env)
        if result.returncode == 0:
            url = result.stdout.strip()
            return url
        else:
            print(f"  gh issue create failed: {result.stderr}", file=sys.stderr)
            return None
    except Exception as e:
        print(f"  gh issue create exception: {e}", file=sys.stderr)
        return None


def save_results(suite_dir: Path, suite_result: SuiteResult, gemalto_msgs: list = None, firmware_msgs: list = None, diffs: list = None):
    suite_dir.mkdir(parents=True, exist_ok=True)
    suite_result.completed_at = datetime.now(timezone.utc).isoformat()

    with open(suite_dir / "summary.json", "w") as f:
        json.dump(suite_result.to_dict(), f, indent=2)

    if gemalto_msgs:
        with open(suite_dir / "gemalto-ccid.json", "w") as f:
            json.dump([m.to_dict() for m in gemalto_msgs], f, indent=2)
    if firmware_msgs:
        with open(suite_dir / "firmware-ccid.json", "w") as f:
            json.dump([m.to_dict() for m in firmware_msgs], f, indent=2)
    if diffs is not None:
        with open(suite_dir / "comparison.json", "w") as f:
            json.dump(diffs, f, indent=2)

    with open(suite_dir / "summary.txt", "w") as f:
        f.write(f"Soak Test Suite: {suite_result.suite_name}\n")
        f.write(f"Started:  {suite_result.started_at}\n")
        f.write(f"Completed: {suite_result.completed_at}\n")
        f.write(f"Results:   {suite_result.passed}/{suite_result.total_tests} passed, "
                f"{suite_result.failed} failed, {suite_result.skipped} skipped\n")
        f.write(f"Bugs:      {suite_result.bugs_found}\n")
        if suite_result.notes:
            f.write(f"\nNotes: {suite_result.notes}\n")
        for r in suite_result.results:
            status = "PASS" if r.passed else "FAIL"
            f.write(f"\n  [{status}] {r.test_name}\n")
            if not r.passed:
                if r.difference:
                    f.write(f"    Difference: {r.difference}\n")
                if r.github_issue_url:
                    f.write(f"    Issue: {r.github_issue_url}\n")


def run_test_on_both(
    suite: SuiteResult,
    test_name: str,
    test_fn,
    readers: dict,
    gemalto_conn=None,
    firmware_conn=None,
    compare_ccid: bool = True,
    normalize=None,
    expect_response_diff: bool = False,
) -> TestResult:
    """Run test_fn on both readers and compare results.

    Args:
        normalize: optional callable(str) -> str to strip reader-specific
                   data before comparing responses (e.g. serial numbers).
        expect_response_diff: if True, response content differences are
                   logged but not treated as failures.
    """
    result = TestResult(suite_name=suite.suite_name, test_name=test_name, passed=True)

    try:
        g_result = test_fn("gemalto", readers["gemalto"], gemalto_conn)
        result.gemalto_ok = True
        result.gemalto_response = str(g_result) if g_result else "OK"
    except Exception as e:
        result.gemalto_ok = False
        result.gemalto_error = str(e)

    try:
        f_result = test_fn("firmware", readers["firmware"], firmware_conn)
        result.firmware_ok = True
        result.firmware_response = str(f_result) if f_result else "OK"
    except Exception as e:
        result.firmware_ok = False
        result.firmware_error = str(e)

    if result.gemalto_ok != result.firmware_ok:
        result.passed = False
        result.difference = (
            f"Gemalto {'OK' if result.gemalto_ok else 'FAIL'}, "
            f"Firmware {'OK' if result.firmware_ok else 'FAIL'}"
        )
        if not result.firmware_ok:
            result.difference += f" | Firmware error: {result.firmware_error}"
        if not result.gemalto_ok:
            result.difference += f" | Gemalto error: {result.gemalto_error}"
    elif not result.gemalto_ok and not result.firmware_ok:
        result.passed = True
        result.notes = "Both failed identically"

    g_resp = result.gemalto_response
    f_resp = result.firmware_response
    if normalize:
        g_resp = normalize(g_resp)
        f_resp = normalize(f_resp)

    if g_resp and f_resp and g_resp != f_resp:
        if expect_response_diff:
            result.notes = f"Expected content difference (different cards/readers): {g_resp[:100]} vs {f_resp[:100]}"
        else:
            result.passed = False
            result.difference = (
                f"Response content differs: "
                f"G={g_resp[:200]} F={f_resp[:200]}"
            )

    suite.total_tests += 1
    suite.results.append(result)
    if result.passed:
        suite.passed += 1
        print(f"  [PASS] {test_name}")
        if result.notes:
            print(f"         (note: {result.notes[:120]})")
    else:
        suite.failed += 1

        body = (
            f"## Soak Test Failure: {test_name}\n\n"
            f"**Suite**: {suite.suite_name}\n"
            f"**Test**: {test_name}\n\n"
            f"### Gemalto Reader ({GEMALTO_SERIAL})\n"
            f"- Status: {'OK' if result.gemalto_ok else 'FAIL'}\n"
            f"- Response: `{result.gemalto_response[:500]}`\n"
            f"- Error: `{result.gemalto_error}`\n\n"
            f"### Firmware Reader ({FIRMWARE_SERIAL})\n"
            f"- Status: {'OK' if result.firmware_ok else 'FAIL'}\n"
            f"- Response: `{result.firmware_response[:500]}`\n"
            f"- Error: `{result.firmware_error}`\n\n"
            f"### Difference\n```\n{result.difference[:1000]}\n```\n\n"
            f"### Environment\n"
            f"- Firmware commit: `{subprocess.getoutput('cd /home/ubuntu/gt/ccid_firmware/mayor/rig && git rev-parse --short HEAD').strip()}`\n"
            f"- Detected during automated soak test\n"
        )

        url = file_github_issue(
            title=f"Soak test: {test_name} — behavioral difference vs Gemalto",
            body=body,
            labels=["soak-test", "bug"],
        )
        if url:
            result.bug_filed = True
            result.github_issue_url = url
            suite.bugs_found += 1
            print(f"  [BUG] Filed: {url}")

    return result


def setup_pcsclite_auth():
    try:
        os.makedirs("/etc/polkit-1/localauthority/50-local.d", exist_ok=True)
        polkit_rule = Path("/etc/polkit-1/localauthority/50-local.d/50-pcscd.pkla")
        if not polkit_rule.exists():
            polkit_rule.write_text(
                "[Allow pcscd access]\n"
                "Identity=unix-user:*\n"
                "Action=org.debian.pcsc-lite.access_pcsc\n"
                "ResultAny=yes\n"
                "ResultInactive=yes\n"
                "ResultActive=yes\n"
            )
            subprocess.run(["sudo", "chmod", "644", str(polkit_rule)], capture_output=True)
    except Exception:
        pass
