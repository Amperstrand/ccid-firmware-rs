"""Negative matrix for the J3R180 lifecycle card-identity gate (#341).

Mission card-safety contract: the fixed org GP keyset may ONLY be used
on a card whose CPLC is ledgered at identity level. These tests pin the
fail-closed behavior — wrong card, missing ledger, malformed CPLC,
malformed ledger entries — plus the unwrap that feeds the gate. They
are pure/host-side: pyscard is faked where needed (CI collection gate
runs in a minimal env, so every hardware import stays inside functions).
"""

import sys
import types
from pathlib import Path

def lifecycle():
    """Import the tool the way the nfc suite does (dir on sys.path)."""
    here = Path(__file__).resolve().parent
    if str(here) not in sys.path:
        sys.path.insert(0, str(here))
    import j3r180_gpg_lifecycle as mod
    return mod


FULL_A = "47909E" + "00" * 39  # 42-byte CPLC value, card A (identity pin)
FULL_B = "47909E" + "11" * 39  # same fabricator prefix, DIFFERENT card
assert len(FULL_A) == 84 and len(FULL_B) == 84


class TestLedgerVerdict:
    def test_missing_ledger_refuses_gp_auth(self):
        m = lifecycle()
        ok, line = m.cplc_ledger_verdict(FULL_A, [], False)
        assert not ok and "unset" in line

    def test_unreadable_cplc_refuses(self):
        m = lifecycle()
        ok, _ = m.cplc_ledger_verdict(None, [FULL_A], False)
        assert not ok

    def test_unreadable_cplc_operator_override_proceeds_with_warn(self):
        m = lifecycle()
        ok, line = m.cplc_ledger_verdict(None, [FULL_A], True)
        assert ok and line.startswith("WARN")

    def test_wrong_card_identity_refused(self):
        m = lifecycle()
        ok, line = m.cplc_ledger_verdict(FULL_B, [FULL_A], False)
        assert not ok and "unknown card" in line

    def test_short_prefix_entry_is_not_an_identity(self):
        # "4790" pins only the fabricator — batch kinship at best; the
        # gate must reject the LEDGER CONFIG, not fall back to it.
        m = lifecycle()
        ok, _ = m.cplc_ledger_verdict(FULL_A, ["4790"], False)
        assert not ok

    def test_nonhex_entry_refused(self):
        m = lifecycle()
        ok, _ = m.cplc_ledger_verdict(FULL_A, ["ZZ" * 20], False)
        assert not ok

    def test_malformed_batch_entry_refused(self):
        m = lifecycle()
        ok, _ = m.cplc_ledger_verdict(FULL_A, ["NOTHEX*"], False)
        assert not ok

    def test_identity_entry_passes(self):
        m = lifecycle()
        ok, line = m.cplc_ledger_verdict(FULL_A, [FULL_B, FULL_A], False)
        assert ok and "ledgered" in line

    def test_declared_batch_prefix_passes_loudly(self):
        m = lifecycle()
        ok, line = m.cplc_ledger_verdict(FULL_B, ["47909E*"], False)
        assert ok and "batch" in line

    def test_malformed_entry_refuses_even_when_another_matches(self):
        # validate-all-then-match: a broken ledger never authorizes.
        m = lifecycle()
        ok, _ = m.cplc_ledger_verdict(FULL_A, [FULL_A, "4790"], False)
        assert not ok


class TestPlainCplcUnwrap:
    """plain_cplc through a faked pyscard connection."""

    def _with_fake(self, data, sw1=0x90, sw2=0x00):
        m = lifecycle()
        fake_conn = types.SimpleNamespace(
            connect=lambda: None,
            transmit=lambda b: (data, sw1, sw2),
            disconnect=lambda: None,
        )
        # Register every module PATH the tool's lazy imports walk:
        # `from smartcard.pcsc.PCSCCardConnection import …` imports a
        # SUBMODULE, so it must exist in sys.modules, not just as an
        # attribute of its parent.
        conn_mod = types.ModuleType("smartcard.pcsc.PCSCCardConnection")
        conn_mod.PCSCCardConnection = lambda reader: fake_conn
        pcsc_mod = types.ModuleType("smartcard.pcsc")
        pcsc_mod.__path__ = []  # noqa: B010 — mark as package
        pcsc_mod.PCSCCardConnection = conn_mod.PCSCCardConnection
        sc_mod = types.ModuleType("smartcard")
        sc_mod.__path__ = []  # noqa: B010
        sc_mod.pcsc = pcsc_mod
        util_mod = types.ModuleType("smartcard.util")
        util_mod.toBytes = lambda h: list(bytes.fromhex(h))
        sc_mod.util = util_mod
        saved = {k: sys.modules.get(k) for k in
                 ("smartcard", "smartcard.pcsc", "smartcard.util",
                  "smartcard.pcsc.PCSCCardConnection")}
        sys.modules.update({"smartcard": sc_mod,
                            "smartcard.pcsc": pcsc_mod,
                            "smartcard.util": util_mod,
                            "smartcard.pcsc.PCSCCardConnection": conn_mod})
        try:
            return m.plain_cplc("FakeReader")
        finally:
            for k, v in saved.items():
                if v is None:
                    sys.modules.pop(k, None)
                else:
                    sys.modules[k] = v

    def test_valid_tlv_unwraps_to_value_hex(self):
        value = bytes.fromhex(FULL_A)
        wire = bytes([0x9F, 0x7F, len(value)]) + value
        assert self._with_fake(list(wire)) == FULL_A.lower()

    def test_truncated_tlv_is_unreadable(self):
        value = bytes.fromhex(FULL_A)
        wire = bytes([0x9F, 0x7F, len(value)]) + value[:10]
        assert self._with_fake(list(wire)) is None

    def test_wrong_tag_is_unreadable_not_raw(self):
        # a garbage/unexpected answer must never reach the ledger
        wire = bytes([0x6E, 0x00])
        assert self._with_fake(list(wire)) is None

    def test_error_status_word_is_unreadable(self):
        value = bytes.fromhex(FULL_A)
        wire = bytes([0x9F, 0x7F, len(value)]) + value
        assert self._with_fake(list(wire), sw1=0x6A, sw2=0x88) is None


class TestStatusOnlyExit:
    def test_clean_lines_exit_zero(self):
        m = lifecycle()
        assert m.status_only_exit(["preflight: ok", "SELECT: PASS"]) == 0

    def test_fail_lines_exit_nonzero(self):
        m = lifecycle()
        assert m.status_only_exit(["preflight FAIL: CPLC unreadable"]) == 7
