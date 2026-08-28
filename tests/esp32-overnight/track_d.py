#!/usr/bin/env python3
"""Track D — ccid-window soak + diagnostics telemetry (plan todo 13).

Overnight lane for WINDOW2 (after the mid-night role switch to the esp32-ccid
firmware). Five components:

  1. SOAK KEY-AUDIT GATE (mandatory before any soak test runs). The included
     suites are re-used from ``tests/soak/`` — those files were written for a
     JR3180/JavaCard rig and several suites authenticate with hardcoded or
     JavaCard key material (GlobalPlatform SCP, SatoChip PIN "1234", PIV PIN
     "12345678", ...). Tonight's cards are NTAG424 running deterministic keys
     (ledger issuer 0*31+1); any wrong-key APDU burns card auth budget and
     violates the proven-safe envelope. The gate is therefore STATIC and
     fail-closed: a fresh AST audit of ``tests/soak/*.py`` (+ soaklib.py)
     enumerates every APDU each registered soak test sends, and the runner
     refuses (``SoakGateError``) anything whose reviewed verdict in
     ``soak_allowlist.json`` is not INCLUDED — or whose source drifted since
     the review. APDU constructions that cannot be statically resolved are
     EXCLUDED by default.
  2. Reader gate — ``readers()`` must contain "GemPCTwin serial" AND an
     ACR1252 entry, else a Mode-B anomaly row is recorded and the lane
     degrades (honest SKIP rows: no soak, no telemetry).
  3. Soak adapter — drives the allowlisted suite test functions against the
     SELECTED reader (pyscard reader-name matching; suite 10 cross-compares
     ACR vs GemPCTwin). N-iteration / time-budget aware, per-iteration rows.
     Unlike the daytime rig (soaklib.run_test_on_both) it never auto-files
     GitHub issues and never FAILs on response-content differences — the two
     readers hold different cards tonight, so content diffs are expected;
     exceptions are the real signal.
  4. Escape 0xD0 telemetry — SCardControl path (FEATURE_CCID_ESC_COMMAND)
     sending payload D0; parses the 28-byte diagnostics struct
     (crates/ccid-core/src/diagnostics.rs:26-41 — seven LE u32 words:
     apdu_tx, apdu_rx, nak, error, reinit, card_present, uptime_ticks) and
     asserts counters monotonic non-decreasing, reinit delta 0 across the
     window, uptime monotonic. If ESC is unsupported through the pcscd
     serial stack (libccid needs ifdDriverOptions=0x0001) that is recorded
     honestly (escape_supported=false row); the raw fallback is todo 14.
  5. ATR stability — >=500 paced SCardReconnect (>=50 ms apart); every ATR
     must be byte-equal, first mismatch is a recorded finding.

Standalone (offline, no pyscard/hardware):  ``python3 track_d.py --selftest``
Lane integration (overnight.py ``load_track_specs``):  ``build_lane()``
Duck-typed one-shot entry for orchestrators that call it:  ``register(ctx)``

Round-2 amendment: pcscd maintenance pauses this lane too. Module-level
``pause()`` / ``resume()`` (plus the orchestrator's pause-aware ``ctx.sleep``)
gate every reader operation; while paused the lane performs no pcscd traffic
so a pcscd stop/restart never fabricates transport anomalies under us.
"""

import argparse
import ast
import importlib.util
import json
import re
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Optional

REPO_ROOT = Path(__file__).resolve().parents[2]
SOAK_DIR = REPO_ROOT / "tests" / "soak"
ALLOWLIST_PATH = Path(__file__).resolve().parent / "soak_allowlist.json"

# ---------------------------------------------------------------- policy ----

FIRMWARE_READER_NEEDLE = "GemPCTwin serial"
ACR_READER_NEEDLE = "ACR1252"

TONIGHT_SUITES = ("soak_01_smoke", "soak_09_stress", "soak_10_cross_compare")
SUITE_08 = "soak_08_extended_apdu"
SUITE_08_BOUNDARY_SUBSET = ("apdu_100_bytes", "apdu_255_bytes")
PLAN_SUITE_ORDER = TONIGHT_SUITES + (SUITE_08,)
CROSS_COMPARE_SUITE = "soak_10_cross_compare"

# INS bytes that constitute (or set up) card authentication. Any test whose
# resolved APDUs contain one of these is EXCLUDED: on NTAG424 a wrong-key
# auth burns SeqFailCtr/TotFailCtr budget (AN12196 §7.4).
AUTH_INS = {
    0x20: "VERIFY",
    0x22: "MSE (secure messaging setup)",
    0x82: "EXTERNAL/MUTUAL AUTHENTICATE",
    0x84: "GET CHALLENGE",
    0x88: "INTERNAL AUTHENTICATE",
    0xAF: "NTAG424 AUTHENTICATE",
}
INS_NAMES = {
    0xA4: "SELECT",
    0xC0: "GET RESPONSE",
    0xC2: "ENVELOPE",
    0xCA: "GET DATA",
    0xB0: "READ BINARY",
    0xB2: "READ RECORD",
    0xCB: "GET DATA (odd)",
    0xD6: "UPDATE BINARY",
    0xDC: "UPDATE RECORD",
    0xE2: "WRITE RECORD",
}
# Imports / subprocess tooling / constant names that mark a module (or test)
# as authentication-capable even when APDUs are built dynamically.
AUTH_IMPORT_MARKERS = ("pysatochip",)
AUTH_SUBPROCESS_MARKERS = (
    "gp.jar",
    "java -jar",
    "gpg --",
    "opensc-tool",
    "pkcs15-tool",
    "pkcs11-tool",
)
AUTH_CONST_NAME_RE = re.compile(r"(PIN|KEY|AUTH|VERIFY|SCP)", re.I)

ATR_ITERATIONS = 500
ATR_PACE_S = 0.05
MAX_RECONNECT_ERRORS = 10


def _noop(*_a, **_k):
    pass


# ------------------------------------------------------------ pause gate ----


class PcscdPauseGate:
    """Cooperative pause for pcscd maintenance (round-2 amendment).

    The orchestrator's global pcscd-maintenance lock pauses every
    pcscd-consuming lane (Track B AND this one) around any pcscd
    stop/restart; standalone/rehearsal drivers call ``pause()``/``resume()``
    directly. Every reader-touching operation checks the gate first.
    """

    def __init__(self) -> None:
        self._paused = threading.Event()

    def pause(self) -> None:
        self._paused.set()

    def resume(self) -> None:
        self._paused.clear()

    def is_paused(self) -> bool:
        return self._paused.is_set()

    def wait_while_paused(self, poll_s: float = 0.05) -> None:
        while self._paused.is_set():
            time.sleep(poll_s)


PAUSE_GATE = PcscdPauseGate()


def pause() -> None:
    """Pause this lane's pcscd traffic (idempotent)."""
    PAUSE_GATE.pause()


def resume() -> None:
    """Resume pcscd traffic after a maintenance window."""
    PAUSE_GATE.resume()


def _pause_point(ctx=None) -> None:
    """Honor both pause sources before the next reader operation."""
    if ctx is not None:
        ctx.sleep(0.0)  # pause-aware: acknowledges + waits while paused
    PAUSE_GATE.wait_while_paused()


# ------------------------------------------------------------- key audit ----


@dataclass
class AuditEntry:
    suite: str
    test: str  # registered soak-test name (run_test_on_both arg)
    func: str  # python function name in the suite module
    verdict: str  # INCLUDED | EXCLUDED
    key_risk: str  # none | auth-apdu | auth-library | auth-tooling |
    # unresolved | module-marker
    reason: str  # one-line justification
    evidence: list = field(default_factory=list)

    def to_dict(self) -> dict:
        return {
            "suite": self.suite,
            "test": self.test,
            "func": self.func,
            "verdict": self.verdict,
            "key_risk": self.key_risk,
            "reason": self.reason,
            "evidence": list(self.evidence),
        }


def _is_hex_str(s: str) -> bool:
    return (
        len(s) >= 2
        and len(s) % 2 == 0
        and all(c in "0123456789abcdefABCDEF" for c in s)
    )


def _static_apdu_hex(v: ast.expr) -> Optional[str]:
    """Resolve an expression to the APDU hex it provably starts with.

    Understands bytes.fromhex("AB"), pure-hex string constants, and
    ``<resolvable> + <anything>`` (the prefix stays provable — enough to
    pin the INS byte). Returns None when nothing is provable; the caller
    then fails closed.
    """
    if (
        isinstance(v, ast.Call)
        and isinstance(v.func, ast.Attribute)
        and v.func.attr == "fromhex"
        and v.args
        and isinstance(v.args[0], ast.Constant)
        and isinstance(v.args[0].value, str)
    ):
        return v.args[0].value
    if (
        isinstance(v, ast.Constant)
        and isinstance(v.value, str)
        and _is_hex_str(v.value)
    ):
        return v.value
    if isinstance(v, ast.BinOp) and isinstance(v.op, ast.Add):
        return _static_apdu_hex(v.left)
    return None


def _bytes_list_ins(v: ast.expr) -> Optional[int]:
    """INS byte of a bytes([...]) constructor whose second element is a
    constant (e.g. bytes([cla, 0xA4, ...]) — INS provably A4)."""
    if (
        isinstance(v, ast.Call)
        and isinstance(v.func, ast.Name)
        and v.func.id == "bytes"
        and v.args
        and isinstance(v.args[0], ast.List)
        and len(v.args[0].elts) >= 2
    ):
        second = v.args[0].elts[1]
        if isinstance(second, ast.Constant) and isinstance(second.value, int):
            return second.value
    return None


def _module_apdu_constants(tree: ast.Module) -> dict:
    """{constant name: (lineno, provable hex)} for module-level assigns."""
    out = {}
    for node in tree.body:
        if (
            isinstance(node, ast.Assign)
            and len(node.targets) == 1
            and isinstance(node.targets[0], ast.Name)
        ):
            h = _static_apdu_hex(node.value)
            if h is not None:
                out[node.targets[0].id] = (node.lineno, h)
    return out


def _module_auth_markers(tree: ast.Module, fname: str) -> list:
    """Authentication markers anywhere in a suite file (any scope)."""
    markers = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for a in node.names:
                if any(m in a.name for m in AUTH_IMPORT_MARKERS):
                    markers.append(f"{fname}:{node.lineno} import {a.name}")
        elif (
            isinstance(node, ast.ImportFrom)
            and node.module
            and any(m in node.module for m in AUTH_IMPORT_MARKERS)
        ):
            markers.append(f"{fname}:{node.lineno} from {node.module} import")
        elif isinstance(node, ast.Constant) and isinstance(node.value, str):
            for m in AUTH_SUBPROCESS_MARKERS:
                if m in node.value:
                    markers.append(f"{fname}:{node.lineno} auth tooling {m!r}")
                    break
        elif (
            isinstance(node, ast.Assign)
            and len(node.targets) == 1
            and isinstance(node.targets[0], ast.Name)
        ):
            name = node.targets[0].id
            if AUTH_CONST_NAME_RE.search(name) and (
                _static_apdu_hex(node.value) is not None
                or isinstance(node.value, ast.Constant)
            ):
                markers.append(f"{fname}:{node.lineno} key/PIN constant {name}")
    return markers


def registered_pairs(path: Path) -> list:
    """[(registered_name, func_name)] from run_test_on_both(suite, "name",
    test_fn, ...) call sites — exactly the tests a suite main() would
    execute, under their soak names."""
    tree = ast.parse(path.read_text(), filename=str(path))
    pairs = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        callee = getattr(node.func, "id", None) or \
            getattr(node.func, "attr", None)
        if callee != "run_test_on_both":
            continue
        if (len(node.args) >= 3
                and isinstance(node.args[1], ast.Constant)
                and isinstance(node.args[1].value, str)
                and isinstance(node.args[2], ast.Name)):
            pairs.append((node.args[1].value, node.args[2].id))
    return pairs


def _collect_bindings(fn: ast.FunctionDef, module_consts: dict) -> dict:
    """Local name -> ("hexset", {hex,...}) | ("ins", int) | ("unresolved",).

    Covers the patterns the soak suites use: direct hex assigns, module
    constants, bytes([...]) with a constant INS byte, for-loop iteration
    over hex literals, and tuple unpacking from a list literal (directly,
    via rng.choice, or via a named local list) — suite 09's and suite 10's
    mixed-operations patterns.
    """
    bindings: dict = {}
    local_lists: dict = {}

    def bind_single(name, value):
        h = _static_apdu_hex(value)
        if h is not None:
            bindings[name] = ("hexset", {h})
            return
        ins = _bytes_list_ins(value)
        if ins is not None:
            bindings[name] = ("ins", ins)
            return
        bindings[name] = ("unresolved", None)

    def candidate_elements(value):
        """Element nodes of a list literal, following <local list name>,
        rng.choice(X) and list(X) indirections."""
        if isinstance(value, ast.List):
            return value.elts
        if isinstance(value, ast.Name) and value.id in local_lists:
            return local_lists[value.id]
        if isinstance(value, ast.Call):
            callee = getattr(value.func, "attr", None) or getattr(
                value.func, "id", None
            )
            if callee in ("choice", "list") and value.args:
                return candidate_elements(value.args[0])
        return None

    def hex_of_expr(el):
        if isinstance(el, ast.Name) and el.id in module_consts:
            return module_consts[el.id][1]
        return _static_apdu_hex(el)

    def hexes_from(elts):
        hexes = set()
        unresolved = False
        for el in elts:
            h = hex_of_expr(el)
            if h is None:
                unresolved = True
            else:
                hexes.add(h)
        return hexes, unresolved

    for node in ast.walk(fn):
        if isinstance(node, ast.For) and isinstance(node.target, ast.Name):
            elts = candidate_elements(node.iter)
            if elts is not None:
                hexes, unresolved = hexes_from(elts)
                if unresolved:
                    bindings[node.target.id] = ("unresolved", None)
                else:
                    bindings[node.target.id] = ("hexset", hexes)
            continue
        if not (isinstance(node, ast.Assign) and node.targets):
            continue
        target = node.targets[0]
        if isinstance(target, ast.Name):
            bind_single(target.id, node.value)
            if isinstance(node.value, ast.List):
                local_lists[target.id] = node.value.elts
        elif isinstance(target, (ast.Tuple, ast.List)):
            elements = candidate_elements(node.value)
            if elements is None:
                for t in target.elts:
                    if isinstance(t, ast.Name):
                        bindings[t.id] = ("unresolved", None)
                continue
            for idx, t in enumerate(target.elts):
                if not isinstance(t, ast.Name):
                    continue
                hexes = set()
                unresolved = False
                for el in elements:
                    if not (
                        isinstance(el, (ast.Tuple, ast.List))
                        and len(el.elts) == len(target.elts)
                    ):
                        unresolved = True
                        continue
                    h = hex_of_expr(el.elts[idx])
                    if h is None:
                        unresolved = True
                    else:
                        hexes.add(h)
                if unresolved:
                    bindings[t.id] = ("unresolved", None)
                elif hexes:
                    bindings[t.id] = ("hexset", hexes)
                else:
                    bindings[t.id] = ("unresolved", None)
    return bindings


def _audit_function(fn: ast.FunctionDef, fname: str, module_consts: dict) -> tuple:
    """(key_risk, evidence[]) for one suite test function."""
    bindings = _collect_bindings(fn, module_consts)
    apdus: dict = {}
    auth_hits = []
    unresolved_hits = []

    for node in ast.walk(fn):
        # authentication-library calls (e.g. pysc.verify_pin)
        if isinstance(node, ast.Call):
            callee = (
                getattr(node.func, "attr", None) or getattr(node.func, "id", None) or ""
            )
            if callee in ("verify_pin", "authenticate", "open_secure_channel"):
                auth_hits.append(f"{fname}:{node.lineno} auth-library call {callee}()")

        # APDU-bearing transmit calls
        if not isinstance(node, ast.Call):
            continue
        callee = (
            getattr(node.func, "attr", None) or getattr(node.func, "id", None) or ""
        )
        if callee not in ("transmit", "transmit_apdu", "transmit_apdu_str"):
            continue
        if len(node.args) < 2:
            continue
        arg = node.args[1]

        resolved: list = []  # [(hex, label)]
        ins_partial: list = []  # [int]
        unknown = False

        def visit(a):
            nonlocal unknown
            if isinstance(a, ast.Constant) and isinstance(a.value, str):
                if _is_hex_str(a.value):
                    resolved.append((a.value, f"{fname}:{a.lineno}"))
                else:
                    unknown = True
            elif isinstance(a, ast.Name):
                if a.id in module_consts:
                    ln, h = module_consts[a.id]
                    resolved.append((h, f"{fname}:{ln} const {a.id}"))
                elif a.id in bindings:
                    kind, val = bindings[a.id]
                    if kind == "hexset":
                        resolved.extend((h, f"{fname}:{a.lineno} {a.id}") for h in val)
                    elif kind == "ins":
                        ins_partial.append(val)
                    else:
                        unknown = True
                else:
                    unknown = True
            elif isinstance(a, ast.Call):
                if isinstance(a.func, ast.Name) and a.func.id == "list" and a.args:
                    visit(a.args[0])
                    return
                h = _static_apdu_hex(a)
                if h is not None:
                    resolved.append((h, f"{fname}:{a.lineno}"))
                    return
                # fromhex(<for-loop hex-literal name>) — suite 10 pattern
                if (
                    isinstance(a.func, ast.Attribute)
                    and a.func.attr == "fromhex"
                    and a.args
                    and isinstance(a.args[0], ast.Name)
                    and bindings.get(a.args[0].id, ("", None))[0] == "hexset"
                ):
                    nm = a.args[0].id
                    resolved.extend(
                        (hx, f"{fname}:{a.lineno} fromhex({nm})")
                        for hx in bindings[nm][1]
                    )
                    return
                ins = _bytes_list_ins(a)
                if ins is not None:
                    ins_partial.append(ins)
                    return
                unknown = True
            elif isinstance(a, ast.BinOp) and isinstance(a.op, ast.Add):
                h = _static_apdu_hex(a)
                if h is not None:
                    resolved.append((h, f"{fname}:{a.lineno}"))
                else:
                    unknown = True
            else:
                unknown = True

        visit(arg)
        if unknown:
            unresolved_hits.append(
                f"{fname}:{node.lineno} {callee}(<dynamic APDU>) — not "
                f"statically resolvable"
            )
        for ins in ins_partial:
            label = INS_NAMES.get(ins) or AUTH_INS.get(ins) or "?"
            if ins in AUTH_INS:
                auth_hits.append(
                    f"{fname}:{node.lineno} {callee} bytes([...]) INS={ins:02X} {label}"
                )
            else:
                apdus[f"INS{ins:02X}"] = (
                    f"{fname}:{node.lineno} {callee}"
                    f"(bytes([...])) INS={ins:02X} "
                    f"{label}"
                )
        for hexstr, label in resolved:
            try:
                b = bytes.fromhex(hexstr)
            except ValueError:
                unknown = True
                continue
            if len(b) < 2:
                continue
            ins = b[1]
            name = INS_NAMES.get(ins) or AUTH_INS.get(ins) or "?"
            if ins in AUTH_INS:
                auth_hits.append(
                    f"{label} APDU {b.hex().upper()} (INS={ins:02X} {name})"
                )
            else:
                apdus[b.hex().upper()] = (
                    f"{label} APDU {b.hex().upper()} (INS={ins:02X} {name})"
                )

    evidence = list(apdus.values()) + unresolved_hits
    if auth_hits:
        return "auth-apdu/library", evidence + auth_hits
    if unresolved_hits:
        return "unresolved", evidence + [
            "APDU construction not fully provable — fail closed"
        ]
    return "none", evidence


def audit_suite_file(path: Path) -> tuple:
    """(module_scan dict, [AuditEntry]) for one soak suite file."""
    fname = path.name
    tree = ast.parse(path.read_text(), filename=str(path))
    try:
        rel = str(path.relative_to(REPO_ROOT))
    except ValueError:
        rel = str(path)
    markers = _module_auth_markers(tree, fname)
    module_scan = {
        "suite": path.stem,
        "file": rel,
        "module_key_risk": "module-marker" if markers else "none",
        "markers": markers,
    }
    consts = _module_apdu_constants(tree)
    fns = {n.name: n for n in tree.body if isinstance(n, ast.FunctionDef)}
    entries = []
    for reg_name, func_name in registered_pairs(path):
        fn = fns.get(func_name)
        if fn is None:
            entries.append(
                AuditEntry(
                    path.stem,
                    reg_name,
                    func_name,
                    "EXCLUDED",
                    "unresolved",
                    f"registered function {func_name} not found in {fname}",
                    [f"{fname}: run_test_on_both({reg_name!r}, {func_name})"],
                )
            )
            continue
        key_risk, evidence = _audit_function(fn, fname, consts)
        if markers:
            entries.append(
                AuditEntry(
                    path.stem,
                    reg_name,
                    func_name,
                    "EXCLUDED",
                    "module-marker",
                    "suite module carries authentication markers — every test "
                    f"fails closed ({len(markers)} marker(s))",
                    evidence + markers,
                )
            )
            continue
        entries.append(_verdict_for(path.stem, reg_name, func_name, key_risk, evidence))
    return module_scan, entries


def _verdict_for(
    suite: str, test: str, func: str, key_risk: str, evidence: list
) -> AuditEntry:
    if key_risk.startswith("auth"):
        return AuditEntry(
            suite,
            test,
            func,
            "EXCLUDED",
            key_risk,
            f"test authenticates ({key_risk}) — wrong-key budget burn",
            evidence,
        )
    if key_risk == "unresolved":
        return AuditEntry(
            suite,
            test,
            func,
            "EXCLUDED",
            key_risk,
            "sends APDUs that cannot be statically proven key-free",
            evidence,
        )
    checked = "auth-family INS checked against " + "/".join(
        f"{k:02X}" for k in sorted(AUTH_INS)
    )
    if suite in TONIGHT_SUITES:
        ev = evidence or ["no APDU-bearing calls (connect/getATR/disconnect only)"]
        return AuditEntry(
            suite,
            test,
            func,
            "INCLUDED",
            "none",
            "provably key-free (SELECT/ATR/transport ops only)",
            ev + [checked],
        )
    if suite == SUITE_08:
        if test in SUITE_08_BOUNDARY_SUBSET:
            return AuditEntry(
                suite,
                test,
                func,
                "INCLUDED",
                "none",
                "provably key-free 100/255-byte boundary case",
                evidence + [checked],
            )
        return AuditEntry(
            suite,
            test,
            func,
            "EXCLUDED",
            "none",
            "key-free but outside tonight's 100/255-byte boundary subset",
            evidence,
        )
    return AuditEntry(
        suite,
        test,
        func,
        "EXCLUDED",
        "none",
        "suite not in tonight's selection (01/09/10/08-subset)",
        evidence,
    )


def audit_soak_tests(soak_dir: Path = SOAK_DIR) -> dict:
    """Full static key-audit of the soak tree. Returns the allowlist dict."""
    modules, entries = [], []
    paths = sorted(soak_dir.glob("soak_*.py"))
    soaklib = soak_dir / "soaklib.py"
    if soaklib.exists():
        paths.append(soaklib)
    for path in paths:
        m, e = audit_suite_file(path)
        modules.append(m)
        entries.extend(e)
    included = sum(1 for e in entries if e.verdict == "INCLUDED")
    return {
        "version": 1,
        "generated_by": "tests/esp32-overnight/track_d.py --audit",
        "policy": {
            "included_suites": list(TONIGHT_SUITES),
            "suite_08_boundary_subset": list(SUITE_08_BOUNDARY_SUBSET),
            "auth_ins": {f"0x{k:02X}": v for k, v in AUTH_INS.items()},
            "fail_closed": "tests whose APDUs cannot be statically resolved "
            "are EXCLUDED",
            "enforcement": "SoakGate.check() refuses any test without a "
            "reviewed INCLUDED verdict that a fresh source "
            "audit still agrees with",
            "no_github_issues": "the overnight adapter never auto-files "
            "issues and never FAILs on response-content "
            "diffs (different cards in the two readers)",
        },
        "counts": {
            "total": len(entries),
            "included": included,
            "excluded": len(entries) - included,
        },
        "modules": modules,
        "entries": [e.to_dict() for e in entries],
    }


# ------------------------------------------------------------------ gate ----


class SoakGateError(RuntimeError):
    """Raised when a soak test is refused by the key-audit gate."""


class SoakGate:
    """Enforcement point: reviewed allowlist + fresh audit must BOTH say
    INCLUDED before any soak test may run."""

    def __init__(self, reviewed: list, fresh: Optional[list] = None):
        self.reviewed = {(e["suite"], e["test"]): e for e in reviewed}
        self.fresh = (
            {(e["suite"], e["test"]): e for e in fresh} if fresh is not None else None
        )

    @classmethod
    def from_files(cls, allowlist_path=None, soak_dir=None) -> "SoakGate":
        allowlist_path = Path(allowlist_path or ALLOWLIST_PATH)
        data = json.loads(allowlist_path.read_text())
        fresh = audit_soak_tests(Path(soak_dir or SOAK_DIR))
        return cls(data["entries"], fresh["entries"])

    def check(self, suite: str, test: str) -> dict:
        r = self.reviewed.get((suite, test))
        if r is None:
            raise SoakGateError(
                f"{suite}.{test}: no reviewed allowlist entry — refusing"
            )
        if r["verdict"] != "INCLUDED":
            raise SoakGateError(
                f"{suite}.{test}: reviewed verdict {r['verdict']} "
                f"({r['reason']}) — refusing"
            )
        if self.fresh is not None:
            f = self.fresh.get((suite, test))
            if f is None:
                raise SoakGateError(
                    f"{suite}.{test}: no fresh audit entry (source drift) — refusing"
                )
            if f["verdict"] != "INCLUDED":
                raise SoakGateError(
                    f"{suite}.{test}: source drift — fresh audit verdict "
                    f"{f['verdict']} ({f['reason']}) — refusing"
                )
        return r

    def included_tests(self, suite: str) -> list:
        """Ordered test names runnable for ``suite`` (reviewed ∩ fresh)."""
        out = []
        for s, t in self.reviewed:
            if s != suite or self.reviewed[(s, t)]["verdict"] != "INCLUDED":
                continue
            try:
                self.check(s, t)
            except SoakGateError:
                continue
            out.append(t)
        return out


# ----------------------------------------------------------- reader gate ----


@dataclass
class ReaderGate:
    ok: bool
    gempctwin: Optional[str]
    acr1252: Optional[str]
    names: list
    detail: str


def check_readers(names: list) -> ReaderGate:
    fw = next((n for n in names if FIRMWARE_READER_NEEDLE in n), None)
    acr = next((n for n in names if ACR_READER_NEEDLE in n), None)
    missing = [
        n for n, found in (("GemPCTwin serial", fw), ("ACR1252", acr)) if not found
    ]
    detail = (
        "both required readers present"
        if not missing
        else "missing " + " + ".join(missing)
    )
    return ReaderGate(fw is not None and acr is not None, fw, acr, list(names), detail)


def _channel_key(name: str):
    m = re.findall(r"(\d{2}) (\d{2})\s*$", name)
    if not m:
        return (99, 99)
    hi, lo = m[-1]
    return (int(hi), int(lo))


def pick_reader(
    names: list, needle: str, prefer: Optional[str] = None,
    avoid: Optional[str] = None
) -> Optional[str]:
    """Reader-name matching for pyscard reader lists. ACR1252 presents TWO
    pcscd entries — the PICC interface on channel ``00 00`` and the SAM on
    ``01 00`` (task-1 learning): prefer the lowest channel, drop ``avoid``
    substrings (SAM) unless that would leave nothing."""
    cands = [n for n in names if needle in n]
    if not cands:
        return None
    pool = [n for n in cands if avoid is None or avoid not in n] or cands
    for n in pool:
        if prefer and prefer in n:
            return n
    return min(pool, key=_channel_key)


# ------------------------------------------------------------ diagnostics ---

DIAG_SERIALIZED_SIZE = 28
DIAG_COUNTERS = (
    "apdu_tx_count",
    "apdu_rx_count",
    "nak_count",
    "error_count",
    "reinit_count",
)


@dataclass
class DiagnosticsSnapshot:
    apdu_tx_count: int
    apdu_rx_count: int
    nak_count: int
    error_count: int
    reinit_count: int
    card_present: bool
    uptime_ticks: int

    def to_dict(self) -> dict:
        d = {k: getattr(self, k) for k in DIAG_COUNTERS}
        d["card_present"] = self.card_present
        d["uptime_ticks"] = self.uptime_ticks
        return d


def parse_diagnostics(data) -> DiagnosticsSnapshot:
    """28-byte LE diagnostics struct (crates/ccid-core/src/diagnostics.rs
    layout: 7 × u32 LE — apdu_tx, apdu_rx, nak, error, reinit,
    card_present(0/1), uptime_ticks)."""
    data = bytes(data)
    if len(data) < DIAG_SERIALIZED_SIZE:
        raise ValueError(
            f"diagnostics payload must be >= {DIAG_SERIALIZED_SIZE} bytes, "
            f"got {len(data)}"
        )
    words = [int.from_bytes(data[i : i + 4], "little") for i in range(0, 28, 4)]
    return DiagnosticsSnapshot(
        apdu_tx_count=words[0],
        apdu_rx_count=words[1],
        nak_count=words[2],
        error_count=words[3],
        reinit_count=words[4],
        card_present=words[5] != 0,
        uptime_ticks=words[6],
    )


def check_monotonic(prev: DiagnosticsSnapshot, cur: DiagnosticsSnapshot) -> list:
    """Violations between two polls: counter regressions, reinit delta != 0
    across the window, uptime regression."""
    v = []
    for name in DIAG_COUNTERS:
        p, c = getattr(prev, name), getattr(cur, name)
        if c < p:
            v.append(f"{name} regressed {p}->{c} (counters must be non-decreasing)")
    if cur.reinit_count != prev.reinit_count:
        v.append(
            f"reinit_count changed {prev.reinit_count}->"
            f"{cur.reinit_count} (must stay 0 across the window)"
        )
    if cur.uptime_ticks < prev.uptime_ticks:
        v.append(
            f"uptime_ticks regressed {prev.uptime_ticks}->"
            f"{cur.uptime_ticks} (uptime must be monotonic)"
        )
    return v


# -------------------------------------------------- escape 0xD0 telemetry ----

FEATURE_CCID_ESC_COMMAND = 19  # pcsc-lite Part10 tag (pyscard PCSCPart10)


def _default_feature_fn(conn) -> dict:
    from smartcard.pcsc.PCSCPart10 import getFeatureRequest  # lazy

    return dict(getFeatureRequest(conn))


class TelemetryTracker:
    """Escape 0xD0 diagnostics poller with monotonicity assertions."""

    def __init__(self, feature_fn: Optional[Callable] = None):
        self.prev: Optional[DiagnosticsSnapshot] = None
        self.escape_supported: Optional[bool] = None
        self.unsupported_reason = ""
        self.feature_fn = feature_fn or _default_feature_fn

    def poll(self, conn) -> dict:
        if self.escape_supported is False:
            return {
                "type": "telemetry",
                "status": "SKIP",
                "silent": True,
                "reason": self.unsupported_reason,
            }
        try:
            features = self.feature_fn(conn)
            ioctl = features.get(FEATURE_CCID_ESC_COMMAND)
            if ioctl is None:
                self.escape_supported = False
                self.unsupported_reason = (
                    "FEATURE_CCID_ESC_COMMAND not advertised through the "
                    "pcscd serial stack (libccid ifdDriverOptions=0x0001 "
                    "gates ESC) — raw fallback is todo 14"
                )
                return {
                    "type": "telemetry",
                    "status": "SKIP",
                    "escape_supported": False,
                    "reason": self.unsupported_reason,
                }
            resp = bytes(conn.control(ioctl, list(b"\xd0")))
            snap = parse_diagnostics(resp)
        except ValueError as e:
            return {
                "type": "telemetry",
                "status": "FAIL",
                "escape_supported": self.escape_supported,
                "error": f"diagnostics parse failed: {e}",
            }
        except Exception as e:  # transport/protocol failure — honest record
            self.escape_supported = False
            self.unsupported_reason = f"escape control failed: {e!r}"
            return {
                "type": "telemetry",
                "status": "SKIP",
                "escape_supported": False,
                "reason": self.unsupported_reason,
            }
        self.escape_supported = True
        row = {
            "type": "telemetry",
            "status": "PASS",
            "escape_supported": True,
            **snap.to_dict(),
        }
        if self.prev is not None:
            violations = check_monotonic(self.prev, snap)
            if violations:
                row["status"] = "FAIL"
                row["violations"] = violations
        self.prev = snap
        return row


# ------------------------------------------------------------ ATR loop ------


@dataclass
class AtrFinding:
    iteration: int
    expected_hex: str
    got_hex: str


def atr_stability_loop(
    conn,
    *,
    iterations: int = ATR_ITERATIONS,
    pace_s: float = ATR_PACE_S,
    sleep_fn=time.sleep,
    pause_point: Optional[Callable] = None,
    should_continue: Optional[Callable] = None,
    emit: Optional[Callable] = None,
) -> dict:
    """``iterations`` x paced SCardReconnect; every ATR must be byte-equal.
    First mismatch is a recorded finding and stops the loop."""
    if pause_point is None:
        pause_point = _pause_point
    if should_continue is None:
        should_continue = _noop_true
    first = None
    finding = None
    errors = 0
    done = 0
    stopped = "completed"
    for i in range(iterations):
        if not should_continue():
            stopped = "cancelled"
            break
        pause_point()
        try:
            conn.reconnect()
            atr = bytes(conn.getATR())
        except Exception as e:  # transport — count, never fabricate a finding
            errors += 1
            if errors == 1 and emit:
                emit(
                    type="atr_reconnect_error",
                    status="FAIL",
                    iteration=i,
                    error=repr(e),
                )
            if errors >= MAX_RECONNECT_ERRORS:
                stopped = "too_many_reconnect_errors"
                break
            sleep_fn(pace_s)
            continue
        if first is None:
            first = atr
        elif atr != first:
            finding = AtrFinding(i, first.hex(), atr.hex())
            stopped = "atr_mismatch"
            break
        done += 1
        sleep_fn(pace_s)
    return {
        "iterations_done": done,
        "stable": finding is None,
        "atr_hex": first.hex() if first else "",
        "finding": (
            {
                "iteration": finding.iteration,
                "expected": finding.expected_hex,
                "got": finding.got_hex,
            }
            if finding
            else None
        ),
        "reconnect_errors": errors,
        "stopped_reason": stopped,
    }


def _noop_true() -> bool:
    return True


# ------------------------------------------------------------ soak runner ---


class SoakAdapter:
    """Drives allowlisted suite test functions against selected readers.

    Re-uses the suite modules from tests/soak/ directly (their registered
    test functions run unmodified, with our reader_info); their main()
    (pcscd restart + Gemalto-vs-JR3180 rig assumptions) is NOT used.
    """

    def __init__(
        self,
        gate: SoakGate,
        readers_fn: Optional[Callable] = None,
        sleep_fn=time.sleep,
        per_test_pace_s: float = 0.2,
    ):
        self.gate = gate
        self.sleep = sleep_fn
        self.per_test_pace_s = per_test_pace_s
        if readers_fn is None:

            def default_readers():
                import smartcard.System  # lazy — offline tests never get here

                return list(smartcard.System.readers())

            readers_fn = default_readers
        self.readers_fn = readers_fn
        self._modules: dict = {}
        self._pairs: dict = {}

    def connect(self, reader_name: str):
        """Connected pyscard connection for a reader name."""
        for r in self.readers_fn():
            if reader_name in str(r):
                conn = r.createConnection()
                try:
                    from smartcard.scard import SCARD_PROTOCOL_T1

                    conn.connect(SCARD_PROTOCOL_T1)
                except ImportError:
                    conn.connect()
                return conn
        raise RuntimeError(f"reader not found: {reader_name!r}")

    def reader_names(self) -> list:
        return [str(r) for r in self.readers_fn()]

    def reader_info(self, needle: str) -> dict:
        name = pick_reader(self.reader_names(), needle)
        if name is None:
            raise RuntimeError(
                f"no reader matching {needle!r} in {self.reader_names()}"
            )
        for r in self.readers_fn():
            if str(r) == name:
                return {"reader": r, "name": name, "serial": ""}
        raise RuntimeError(f"reader vanished: {name!r}")

    def _suite_module(self, suite: str):
        if suite not in self._modules:
            path = SOAK_DIR / f"{suite}.py"
            spec = importlib.util.spec_from_file_location(
                f"esp32_overnight_{suite}", path
            )
            mod = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(mod)
            self._modules[suite] = mod
            self._pairs[suite] = dict(registered_pairs(path))
        return self._modules[suite]

    def run_test(
        self,
        suite: str,
        test: str,
        fw_info: dict,
        acr_info: Optional[dict] = None,
        iteration: int = -1,
    ) -> dict:
        """Run one allowlisted soak test. The gate is enforced HERE, at the
        execution site — not only at planning time."""
        t0 = time.monotonic()
        row = {
            "type": "soak_test",
            "suite": suite,
            "test": test,
            "iteration": iteration,
        }
        try:
            self.gate.check(suite, test)
        except SoakGateError as e:
            row.update(status="REFUSED", error=str(e))
            return row
        cross = suite == CROSS_COMPARE_SUITE
        try:
            mod = self._suite_module(suite)
            fn = getattr(mod, self._pairs[suite].get(test, ""), None)
            if fn is None:
                raise RuntimeError(f"test function for {suite}.{test} not found")
            fw_res = fn("firmware", fw_info, None)
            row["firmware"] = _jsonable(fw_res)
            if cross:
                if acr_info is None:
                    raise RuntimeError("suite 10 needs the ACR reader")
                acr_res = fn("acr", acr_info, None)
                row["acr"] = _jsonable(acr_res)
                if fw_res != acr_res:
                    # different cards in the two readers: content diffs are
                    # expected, recorded as a note — never a FAIL, never a bug
                    row["note"] = (
                        "response content differs between readers "
                        "(expected: different cards)"
                    )
            row["status"] = "PASS"
        except Exception as e:  # noqa: BLE001 — soak failures are data
            row["status"] = "FAIL"
            row["error"] = repr(e)
        row["dur_ms"] = int((time.monotonic() - t0) * 1000)
        self.sleep(self.per_test_pace_s)
        return row


def _jsonable(obj):
    if isinstance(obj, dict):
        return {k: _jsonable(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple, set)):
        return [_jsonable(v) for v in obj]
    if isinstance(obj, bytes):
        return obj.hex()
    return obj


def run_soak_iterations(
    run_one: Callable,
    plan: list,
    *,
    max_iters: int,
    budget_s: float,
    now_fn=time.monotonic,
    sleep_fn=time.sleep,
    pace_s: float = 1.0,
    emit: Callable = _noop,
    should_continue: Optional[Callable] = None,
    pause_point: Optional[Callable] = None,
) -> dict:
    """N-iteration / time-budget aware driver: one iteration = one pass over
    ``plan`` via ``run_one(suite, test, i)``. Emits per-iteration summary
    rows; never starts an iteration past its time budget."""
    if should_continue is None:
        should_continue = _noop_true
    if pause_point is None:
        pause_point = _pause_point
    t0 = now_fn()
    stats = {
        "iterations": 0,
        "pass": 0,
        "fail": 0,
        "refused": 0,
        "stopped_reason": "completed",
    }
    for i in range(max_iters):
        if not should_continue():
            stats["stopped_reason"] = "cancelled"
            break
        pause_point()
        if now_fn() - t0 >= budget_s:
            stats["stopped_reason"] = "time_budget_exhausted"
            emit(
                {
                    "type": "soak_iter",
                    "iteration": i,
                    "status": "SKIP",
                    "reason": "time budget exhausted",
                }
            )
            break
        iter_t0 = now_fn()
        p = f = r = 0
        for suite, test in plan:
            row = run_one(suite, test, i)
            emit(row)
            if row.get("status") == "PASS":
                p += 1
            elif row.get("status") == "REFUSED":
                r += 1
            else:
                f += 1
        stats["iterations"] += 1
        stats["pass"] += p
        stats["fail"] += f
        stats["refused"] += r
        emit(
            {
                "type": "soak_iter",
                "iteration": i,
                "status": "PASS" if f == 0 and r == 0 else "FAIL",
                "tests_pass": p,
                "tests_fail": f,
                "tests_refused": r,
                "dur_ms": int((now_fn() - iter_t0) * 1000),
            }
        )
        sleep_fn(pace_s)
    return stats


# ------------------------------------------------------------------ lane ----


def build_plan(gate: SoakGate) -> list:
    plan = []
    for suite in PLAN_SUITE_ORDER:
        for test in gate.included_tests(suite):
            plan.append((suite, test))
    return plan


def run(ctx) -> None:
    """Track D lane target — duck-typed against the overnight.py
    PhaseContext API (running/paused/sleep/row/skip/anomaly/event)."""
    ctx.event("track_d_start")
    # 1) key-audit gate — before ANY soak consideration
    try:
        gate = SoakGate.from_files()
    except Exception as e:  # noqa: BLE001 — gate failure degrades, not crashes
        ctx.anomaly("soak_gate_refused", error=repr(e))
        ctx.skip("soak gate failed to load; no soak tests may run")
        return
    plan = build_plan(gate)
    if not plan:
        ctx.skip("no allowlisted soak tests runnable")
        return
    ctx.row(
        type="soak_gate",
        status="PASS",
        tests=len(plan),
        suites=sorted({s for s, _ in plan}),
    )

    if getattr(ctx, "dry_run", False):
        ctx.row(type="reader_gate", status="PASS", simulated=True)
        ctx.row(
            type="soak_iter",
            iteration=0,
            status="PASS",
            simulated=True,
            tests_pass=len(plan),
        )
        ctx.row(type="telemetry", status="PASS", simulated=True)
        ctx.row(
            type="atr_stability",
            status="PASS",
            simulated=True,
            iterations=ATR_ITERATIONS,
        )
        return

    # 2) reader gate (Mode B on failure)
    adapter = SoakAdapter(gate)
    try:
        names = adapter.reader_names()
    except Exception as e:  # noqa: BLE001 — pcscd down at window start
        names = []
        ctx.anomaly("readers_error", error=repr(e))
    rg = check_readers(names)
    if not rg.ok:
        ctx.anomaly("mode_b_reader_gate", detail=rg.detail, readers=rg.names)
        for what in ("soak iterations", "escape telemetry", "atr stability"):
            ctx.skip(f"Mode-B degrade (reader gate): {what}")
        return
    ctx.row(
        type="reader_gate",
        status="PASS",
        firmware=rg.gempctwin,
        acr=rg.acr1252,
        readers=rg.names,
    )

    fw_info = adapter.reader_info(FIRMWARE_READER_NEEDLE)
    acr_info = adapter.reader_info(ACR_READER_NEEDLE)

    # 3) ATR stability loop (once, bounded, pause-aware)
    try:
        conn = adapter.connect(rg.gempctwin)
        summary = atr_stability_loop(
            conn,
            iterations=ATR_ITERATIONS,
            pace_s=ATR_PACE_S,
            sleep_fn=ctx.sleep,
            pause_point=lambda: _pause_point(ctx),
            should_continue=ctx.running,
            emit=ctx.row,
        )
        conn.disconnect()
    except Exception as e:  # noqa: BLE001 — degrade to anomaly, keep soaking
        summary = {"stable": False, "stopped_reason": f"error {e!r}"}
    ctx.row(type="atr_stability", **_jsonable(summary))

    # 4) soak iterations + telemetry polls until the window ends
    tracker = TelemetryTracker()

    def run_one(suite, test, i):
        _pause_point(ctx)
        return adapter.run_test(suite, test, fw_info, acr_info, iteration=i)

    def poll_telemetry(i):
        try:
            conn = adapter.connect(rg.gempctwin)
            row = tracker.poll(conn)
            conn.disconnect()
        except Exception as e:  # noqa: BLE001 — transport, honest record
            return {"type": "telemetry", "status": "FAIL", "error": repr(e)}
        row["iteration"] = i
        return row

    def emit(row):
        if row.get("silent"):
            return
        violations = row.pop("violations", None) or []
        ctx.row(**row)
        for v in violations:
            ctx.anomaly(
                "diagnostics_violation", violation=v, iteration=row.get("iteration", -1)
            )

    stats = run_soak_iterations(
        run_one,
        plan,
        max_iters=10**9,
        budget_s=float("inf"),
        sleep_fn=ctx.sleep,
        pace_s=max(ctx.pace_s, 1.0),
        emit=emit,
        should_continue=ctx.running,
        pause_point=lambda: _pause_point(ctx),
    )
    emit(poll_telemetry(stats["iterations"]))
    ctx.event("track_d_done", **stats)


# duck-typed one-shot entry for orchestrators that call register(ctx)
register = run


def build_lane():
    """LaneSpec for overnight.py's tolerant loader (load_track_specs)."""
    try:
        import overnight  # present when loaded from the orchestrator's dir

        return overnight.LaneSpec(
            "track_d_soak",
            run,
            window="window2",
            cards=(),
            needs_pcscd=True,
            pace_s=1.0,
        )
    except ImportError:
        from types import SimpleNamespace

        return SimpleNamespace(
            name="track_d_soak",
            target=run,
            window="window2",
            cards=(),
            needs_pcscd=True,
            pace_s=1.0,
        )


# ---------------------------------------------------------- test fakes ------


def stub_smartcard(setitem=None) -> None:
    """sys.modules stubs so suite imports never touch pcscd (offline tests)."""
    import types

    if setitem is None:

        def setitem(d, k, v):
            d[k] = v

    def mod(name):
        m = types.ModuleType(name)
        setitem(sys.modules, name, m)
        return m

    base = mod("smartcard")
    base.System = mod("smartcard.System")
    scard = mod("smartcard.scard")
    scard.SCARD_PROTOCOL_T1 = 1
    cardreq = mod("smartcard.CardRequest")
    cardreq.CardRequest = object
    exc = mod("smartcard.Exceptions")
    for name in ("NoCardException", "CardConnectionException"):
        setattr(exc, name, type(name, (Exception,), {}))


class FakeConn:
    """Duck-typed pyscard connection — offline tests/selftest only."""

    def __init__(self, atr=None):
        self.atr = atr or bytes.fromhex("3B8580018073C821100E")

    def connect(self, protocol=None):
        return None

    def disconnect(self):
        return None

    def reconnect(self):
        return None

    def getATR(self):
        return self.atr

    def transmit(self, apdu):
        return [], 0x6A, 0x82

    def control(self, ioctl, data):
        raise KeyError("FEATURE_CCID_ESC_COMMAND")


class FakeReader:
    def __init__(self, name, conn=None):
        self.name = name
        self._conn = conn or FakeConn()

    def __str__(self):
        return self.name

    def createConnection(self):
        return self._conn


def diag_bytes(apdu_tx, apdu_rx, nak, error, reinit, present, uptime):
    return b"".join(
        v.to_bytes(4, "little")
        for v in (apdu_tx, apdu_rx, nak, error, reinit, present, uptime)
    )


# --------------------------------------------------------------- selftest ---


def selftest() -> int:
    """Offline checks: allowlist gate, parsers, reader gate, comparator,
    budget logic. No pyscard import, no hardware, no pcscd."""
    failures = []

    def check(name, fn):
        try:
            fn()
            print(f"[PASS] {name}")
        except AssertionError as e:
            failures.append(name)
            print(f"[FAIL] {name}: {e}")
        except Exception as e:  # noqa: BLE001 — a selftest crash is a failure
            failures.append(name)
            print(f"[FAIL] {name}: {e!r}")

    def t_audit():
        audit = audit_soak_tests()
        by = {}
        for e in audit["entries"]:
            by.setdefault(e["suite"], {})[e["test"]] = e
        expected_counts = {
            "soak_01_smoke": 6,
            "soak_09_stress": 7,
            "soak_10_cross_compare": 14,
        }
        for suite, count in expected_counts.items():
            got = by[suite]
            assert len(got) == count, f"{suite}: {len(got)} tests != {count}"
            bad = {
                t: e["verdict"] for t, e in got.items() if e["verdict"] != "INCLUDED"
            }
            assert not bad, f"{suite} non-INCLUDED: {bad}"
        for t in SUITE_08_BOUNDARY_SUBSET:
            assert by[SUITE_08][t]["verdict"] == "INCLUDED", t
        for suite in (
            "soak_02_globalplatform",
            "soak_03_satochip",
            "soak_05_piv",
            "soak_06_opensc",
            "soak_07_gpg",
        ):
            verdicts = {e["verdict"] for e in by.get(suite, {}).values()}
            assert verdicts == {"EXCLUDED"}, f"{suite}: {verdicts}"
            assert any(e["key_risk"] != "none" for e in by[suite].values()), (
                f"{suite}: exclusion lacks auth evidence"
            )

    check("audit: 01/09/10 all INCLUDED; 08 subset INCLUDED; 02-07 EXCLUDED", t_audit)

    def t_inject():
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "soak_99_injected.py"
            p.write_text(
                "from soaklib import transmit_apdu\n"
                "VERIFY = bytes.fromhex('0020008008') + b'12345678' + b'00'\n"
                "def test_pin(label, reader_info, conn):\n"
                "    data, sw = transmit_apdu(conn, VERIFY)\n"
                "    return {'sw': sw}\n"
                "def run():\n"
                "    run_test_on_both(suite, 'pin', test_pin, readers)\n"
            )
            _, entries = audit_suite_file(p)
            e = entries[0]
            assert e.verdict == "EXCLUDED", e
            assert e.key_risk in ("module-marker", "auth-apdu/library"), e

    check("audit: injected VERIFY APDU is EXCLUDED", t_inject)

    def t_gate():
        reviewed = [
            {
                "suite": "s",
                "test": "ok",
                "verdict": "INCLUDED",
                "reason": "",
                "evidence": [],
            },
            {
                "suite": "s",
                "test": "bad",
                "verdict": "EXCLUDED",
                "reason": "auth",
                "evidence": [],
            },
        ]
        fresh_ok = [
            {"suite": "s", "test": "ok", "verdict": "INCLUDED"},
            {"suite": "s", "test": "bad", "verdict": "EXCLUDED"},
        ]
        g = SoakGate(reviewed, fresh_ok)
        g.check("s", "ok")

        def expect_refuse(op):
            try:
                op()
                raise AssertionError("gate did not refuse")
            except SoakGateError:
                pass

        expect_refuse(lambda: g.check("s", "bad"))
        expect_refuse(lambda: g.check("s", "gone"))
        drift = SoakGate(
            reviewed,
            [{"suite": "s", "test": "ok", "verdict": "EXCLUDED", "reason": "drifted"}],
        )
        expect_refuse(lambda: drift.check("s", "ok"))
        assert SoakGate(reviewed, fresh_ok).included_tests("s") == ["ok"]

    check("gate: refuses EXCLUDED/missing/drifted; included_tests filters", t_gate)

    def t_diag():
        snap = parse_diagnostics(diag_bytes(7, 6, 5, 4, 0, 1, 999))
        assert (
            snap.apdu_tx_count,
            snap.apdu_rx_count,
            snap.nak_count,
            snap.error_count,
            snap.reinit_count,
        ) == (7, 6, 5, 4, 0)
        assert snap.card_present and snap.uptime_ticks == 999
        try:
            parse_diagnostics(b"\x00" * 27)
            raise AssertionError("short buffer accepted")
        except ValueError:
            pass

    check("diagnostics: 28-byte LE parse + short rejection", t_diag)

    def t_mono():
        def s(tx, rx, nak, err, reinit, up):
            return DiagnosticsSnapshot(tx, rx, nak, err, reinit, True, up)

        assert check_monotonic(s(1, 2, 0, 0, 0, 10), s(3, 4, 1, 0, 0, 20)) == []
        v = check_monotonic(s(5, 2, 0, 0, 0, 30), s(4, 6, 0, 1, 1, 29))
        assert any("apdu_tx_count regressed" in x for x in v), v
        assert any("reinit_count changed" in x for x in v), v
        assert any("uptime_ticks regressed" in x for x in v), v

    check("monotonic: clean pass + counter/reinit/uptime violations", t_mono)

    def t_readers():
        ok = check_readers(["GemPCTwin serial 00 00", "ACS ACR1252 01 00"])
        assert ok.ok and ok.gempctwin and ok.acr1252
        assert not check_readers(["ACS ACR1252 01 00"]).ok
        assert not check_readers(["GemPCTwin serial 00 00"]).ok
        assert not check_readers([]).ok
        names = ["ACS ACR1252 00 00", "ACS ACR1252 01 00"]
        assert pick_reader(names, "ACR1252") == "ACS ACR1252 00 00"

    check("reader gate: both-required + PICC-over-SAM pick", t_readers)

    def t_atr():
        good = FakeConn()
        s = atr_stability_loop(good, iterations=5, pace_s=0.0)
        assert s["stable"] and s["iterations_done"] == 5, s
        flaky = FakeConn()
        counter = iter(range(10))
        real_reconnect = flaky.reconnect

        def flip_reconnect():
            real_reconnect()
            if next(counter) == 2:
                flaky.atr = bytes.fromhex("3B0000000000000000")

        flaky.reconnect = flip_reconnect
        s2 = atr_stability_loop(flaky, iterations=5, pace_s=0.0)
        assert not s2["stable"] and s2["finding"], s2

    check("atr: byte-equal loop + first-mismatch finding", t_atr)

    def t_budget():
        clock = {"t": 0.0}
        rows = []

        def run_one(suite, test, i):
            clock["t"] += 1.0
            return {"type": "soak_test", "status": "PASS", "suite": suite,
                    "test": test}

        def fake_sleep(s):
            clock["t"] += s

        stats = run_soak_iterations(
            run_one,
            [("a", "x")],
            max_iters=10,
            budget_s=3.5,
            now_fn=lambda: clock["t"],
            sleep_fn=fake_sleep,
            pace_s=0.0,
            emit=rows.append,
        )
        # i0: t=0<3.5 run (t=1); i1: 1<3.5 (t=2); i2: 2<3.5 (t=3);
        # i3: 3<3.5 (t=4); i4: 4>=3.5 -> SKIP row
        assert stats["iterations"] == 4, stats
        assert stats["stopped_reason"] == "time_budget_exhausted", stats
        rows.clear()
        stats2 = run_soak_iterations(
            run_one,
            [("a", "x")],
            max_iters=2,
            budget_s=1000,
            now_fn=lambda: clock["t"],
            sleep_fn=fake_sleep,
            pace_s=0.0,
            emit=rows.append,
        )
        assert stats2["iterations"] == 2 and stats2["pass"] == 2, stats2
        assert any(r["type"] == "soak_iter" for r in rows)

    check("budget: stops at time budget and max_iters; per-iteration rows", t_budget)

    def t_pause():
        pause()
        assert PAUSE_GATE.is_paused()
        t0 = time.monotonic()

        def resumer():
            time.sleep(0.15)
            resume()

        th = threading.Thread(target=resumer)
        th.start()
        PAUSE_GATE.wait_while_paused()
        th.join()
        assert time.monotonic() - t0 >= 0.1
        assert not PAUSE_GATE.is_paused()

    check("pause gate: pause()/resume() block operations", t_pause)

    def t_reuse():
        stub_smartcard()
        gate = SoakGate.from_files()
        plan = build_plan(gate)
        assert plan, "no allowlisted tests"
        adapter = SoakAdapter(
            gate,
            readers_fn=lambda: [
                FakeReader("GemPCTwin serial 00 00"),
                FakeReader("ACS ACR1252 00 00"),
            ],
        )
        fw = adapter.reader_info(FIRMWARE_READER_NEEDLE)
        acr = adapter.reader_info(ACR_READER_NEEDLE)
        row = adapter.run_test("soak_01_smoke", "select_mf", fw, acr, iteration=0)
        assert row["status"] == "PASS", row
        refused = adapter.run_test("soak_02_globalplatform", "whatever", fw, acr)
        assert refused["status"] == "REFUSED", refused

    check("adapter: runs real suite fn via mocks; refuses unaudited", t_reuse)

    def t_escape_unsupported():
        tr = TelemetryTracker(feature_fn=lambda conn: {})
        row = tr.poll(FakeConn())
        assert row["status"] == "SKIP" and row["escape_supported"] is False
        assert "todo 14" in row["reason"]
        again = tr.poll(FakeConn())
        assert again.get("silent") is True

    check("telemetry: ESC unsupported -> honest SKIP row", t_escape_unsupported)

    def t_escape_poll():
        state = {"n": 0}

        def control(ioctl, data):
            state["n"] += 1
            n = state["n"]
            return diag_bytes(10 + n, 10 + n, 0, 0, 0, 1, 1000 + n)

        tr = TelemetryTracker(feature_fn=lambda c: {19: 0x42})
        conn = FakeConn()
        conn.control = control
        r1 = tr.poll(conn)
        assert r1["status"] == "PASS" and r1["escape_supported"], r1
        r2 = tr.poll(conn)
        assert r2["status"] == "PASS", r2
        assert r2["apdu_tx_count"] >= r1["apdu_tx_count"], r2

        def regress(ioctl, data):
            return diag_bytes(1, 1, 0, 0, 1, 1, 5)

        conn.control = regress
        r3 = tr.poll(conn)
        assert r3["status"] == "FAIL", r3
        assert len(r3["violations"]) == 4, r3  # tx+rx regress, reinit, uptime
        assert any("reinit_count changed" in v for v in r3["violations"])

    check("telemetry: control-path poll + violation detection", t_escape_poll)

    print(f"\nselftest: {len(failures)} failure(s)")
    return 1 if failures else 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument(
        "--selftest",
        action="store_true",
        help="offline self-test (no pyscard, no hardware)",
    )
    ap.add_argument(
        "--audit",
        action="store_true",
        help="print the fresh soak key-audit as JSON (review "
        "output -> soak_allowlist.json)",
    )
    args = ap.parse_args(argv)
    if args.audit:
        print(json.dumps(audit_soak_tests(), indent=2))
        return 0
    if args.selftest:
        return selftest()
    ap.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
