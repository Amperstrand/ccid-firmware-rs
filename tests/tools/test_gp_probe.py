"""Offline gates for host-tools/gp_probe.py (#139 GP conformance lane).

Pure stdlib+pytest (python-collect minimal env): the read-only allowlist,
the bench-locked APDU bytes, and the offline CPLC / INITIALIZE UPDATE
decoders. Hardware execution is exercised through a faked pyscard
(the real one stays untouched); the dry-run default is proven with an
import trap.
"""

import json
import sys
import types
from pathlib import Path
from sys import path as sys_path

REPO = Path(__file__).resolve().parents[2]
sys_path.insert(0, str(REPO / "host-tools"))
import gp_probe as gpp  # noqa: E402

# Bench-locked APDU bytes (GREAT EXPECTATION style — if these break,
# re-verify against the citations, never "fix" the test):
#   SELECT ISD form: tests/hardware/nfc/cardlib.py select_aid +
#   j3r180 lifecycle OPENPGP_SELECT shape (00A40400 Lc AID, no Le)
#   GET DATA CPLC form: j3r180_gpg_lifecycle.py:121 (80CA9F7F00) and
#   GlobalPlatformPro GPData.java:366-368 (case-2, Le=256)
#   GET STATUS form: GPPro GPSession.java:1043-1048 P1=0x80 P2=0x02
#   data 4F00 Le=256
SELECT_HEX = "00a4040008a000000151000000"
CPLC_HEX = "80ca9f7f00"
GET_STATUS_HEX = "80f28002024f0000"

# Synthetic CPLC values in the bench capture shape (#155 / #127):
# ICS fab/type, OS id/date/level, ICF fab(1)/type(1), serial(4), batch(2)
CPLC_50906420 = "4790d3214700000000003129509064206782" + "00" * 24
CPLC_50942720 = "4790d3214700000000003129509427206782" + "00" * 24


class TestGuardAllowlist:
    def test_select_isd_allowed(self):
        ok, _ = gpp.guard_apdu(gpp.APDU_SELECT_ISD)
        assert ok

    def test_cplc_get_data_allowed_both_cla(self):
        for cla in (0x80, 0x00):
            ok, _ = gpp.guard_apdu(bytes([cla, 0xCA, 0x9F, 0x7F, 0x00]))
            assert ok

    def test_init_update_rejected(self):
        apdu = bytes.fromhex("8050000008") + bytes(8)
        ok, reason = gpp.guard_apdu(apdu)
        assert not ok and "doctrine" in reason

    def test_external_authenticate_rejected(self):
        apdu = bytes.fromhex("8482010008") + bytes(8)
        ok, _ = gpp.guard_apdu(apdu)
        assert not ok

    def test_state_changing_gp_commands_rejected(self):
        for ins in (0xD8, 0xE6, 0xE8, 0xE2):  # PUT KEY / INSTALL / LOAD / STORE DATA
            ok, _ = gpp.guard_apdu(bytes([0x80, ins, 0x00, 0x00]))
            assert not ok

    def test_get_data_00e0_is_doctrine_sc_class(self):
        # keyless in GPPro but SC-class for this fleet: the locked card
        # answers 69FF to it (#144)
        ok, _ = gpp.guard_apdu(bytes.fromhex("00cae00000"))
        assert not ok

    def test_other_get_data_tags_rejected(self):
        ok, _ = gpp.guard_apdu(bytes.fromhex("80ca9f7e00"))
        assert not ok

    def test_get_status_needs_flag(self):
        ok, _ = gpp.guard_apdu(gpp.APDU_GET_STATUS_ISD)
        assert not ok
        ok, reason = gpp.guard_apdu(gpp.APDU_GET_STATUS_ISD, allow_get_status=True)
        assert ok and "opt-in" in reason

    def test_unknown_header_rejected(self):
        ok, _ = gpp.guard_apdu(bytes.fromhex("0020000004") + bytes(4))  # VERIFY
        assert not ok

    def test_short_buffer_rejected(self):
        ok, _ = gpp.guard_apdu(bytes.fromhex("80ca"))
        assert not ok


class TestBenchLockedApduBytes:
    def test_select_isd_bytes(self):
        assert gpp.APDU_SELECT_ISD.hex() == SELECT_HEX

    def test_cplc_bytes(self):
        assert gpp.APDU_GET_DATA_CPLC.hex() == CPLC_HEX

    def test_get_status_bytes(self):
        assert gpp.APDU_GET_STATUS_ISD.hex() == GET_STATUS_HEX

    def test_probes_all_pass_their_own_guard(self):
        for probe in gpp.PROBES:
            if probe["needs_flag"]:
                continue
            ok, _ = gpp.guard_apdu(probe["apdu"])
            assert ok, probe["name"]


class TestParseCplc:
    def test_bench_shape_50906420(self):
        f = gpp.parse_cplc(CPLC_50906420)
        assert f["icf_serial"] == "50906420"
        assert f["icf_batch"] == "6782"
        assert f["registry_key"] == "50906420/6782"
        assert f["ics_fabricator"] == "4790" and f["ics_type"] == "d321"

    def test_bench_shape_50942720(self):
        assert gpp.parse_cplc(CPLC_50942720)["registry_key"] == "50942720/6782"

    def test_identity_pin_is_32_hex(self):
        f = gpp.parse_cplc(CPLC_50906420)
        assert len(f["identity_hex"]) == 32

    def test_too_short_raises(self):
        import pytest

        with pytest.raises(ValueError):
            gpp.parse_cplc("4790d321")

    def test_nonhex_raises(self):
        import pytest

        with pytest.raises(ValueError):
            gpp.parse_cplc("ZZ" * 21)


class TestUnwrapCplc:
    def test_valid_tlv(self):
        value = bytes.fromhex(CPLC_50906420)
        wire = bytes([0x9F, 0x7F, len(value)]) + value
        assert gpp.unwrap_cplc_tlv(wire) == value

    def test_wrong_tag_is_unreadable(self):
        assert gpp.unwrap_cplc_tlv(bytes.fromhex("6e00")) is None

    def test_truncated_length_is_unreadable(self):
        value = bytes.fromhex(CPLC_50906420)
        wire = bytes([0x9F, 0x7F, len(value)]) + value[:10]
        assert gpp.unwrap_cplc_tlv(wire) is None


def _scp02_response(seq="0433", challenge="AABBCCDDEEFF",
                    cryptogram="1122334455667788", kdd="4790d321509427206782",
                    kvn="ff"):
    return (kdd + kvn + "02" + seq + challenge + cryptogram)


KDD_10B = "4790d321470000000000"  # 10-byte key diversification data


class TestParseInitUpdate:
    def test_scp02_28b(self):
        r = gpp.parse_init_update(_scp02_response())
        assert r["kdd"] == "4790D321509427206782"
        assert r["key_version"] == "FF"
        assert r["scp"] == "02"
        assert r["sequence_counter"] == "0433"
        assert r["card_challenge"] == "AABBCCDDEEFF"
        assert r["card_cryptogram"] == "1122334455667788"
        assert len(r["card_challenge"]) == 12  # 6 bytes — the SCP02 shape
        assert len(r["card_cryptogram"]) == 16  # 8 bytes

    def test_bench_i_fingerprint_offset_pinned(self):
        # the "#127 i-params" label = response bytes 12..15 (doc §1.1)
        r = gpp.parse_init_update(_scp02_response(seq="0433", challenge="F5DBAABBCCDD"))
        assert r["bench_i_fingerprint_127"] == "0433F5DB"

    def test_length_deviation_is_an_error_and_names_itself(self):
        import pytest

        # 27B trips the min-length guard; 29B trips the SCP02 exact-length
        # check whose message names the transport-finding class
        with pytest.raises(ValueError, match="too short|transport finding"):
            gpp.parse_init_update(_scp02_response()[:-2])  # 27B
        with pytest.raises(ValueError, match="transport finding"):
            gpp.parse_init_update(_scp02_response() + "00")  # 29B

    def test_scp03_s8_with_pseudorandom(self):
        # i=0x10: pseudorandom challenge (+3B seq) — 29+3 = 32B
        resp = KDD_10B + "ff" + "03" + "10" + "AA" * 8 + "BB" * 8 + "CC" * 3
        r = gpp.parse_init_update(resp)
        assert r["scp"] == "03" and r["i"] == "10"
        assert r["pseudorandom_challenge"] and not r["s16"]
        assert r["sequence_counter"] == "CCCCCC"

    def test_scp03_s16(self):
        # i=0x01: S16 challenges — 29+16 = 45B
        resp = KDD_10B + "ff" + "03" + "01" + "AA" * 16 + "BB" * 16
        r = gpp.parse_init_update(resp)
        assert r["s16"] and not r["pseudorandom_challenge"] and r["length"] == 45

    def test_scp03_s16_pseudorandom(self):
        # i=0x11: both bits — 29+3+16 = 48B
        resp = KDD_10B + "ff" + "03" + "11" + "AA" * 16 + "BB" * 16 + "CC" * 3
        r = gpp.parse_init_update(resp)
        assert r["s16"] and r["pseudorandom_challenge"] and r["length"] == 48

    def test_unsupported_scp_raises(self):
        import pytest

        resp = KDD_10B + "ff" + "04" + "00" * 16
        with pytest.raises(ValueError, match="unsupported SCP"):
            gpp.parse_init_update(resp)

    def test_too_short_raises(self):
        import pytest

        with pytest.raises(ValueError):
            gpp.parse_init_update("00" * 10)


class _FakeConn:
    def __init__(self, answers):
        self._answers = answers
        self.connected = False

    def connect(self):
        self.connected = True

    def getATR(self):
        return list(bytes.fromhex("3B8580018073C821100E"))

    def transmit(self, apdu):
        return self._answers[bytes(apdu).hex()]

    def disconnect(self):
        self.connected = False


class _FakeReader:
    def __str__(self):
        return "FakeReader 00 00"

    def createConnection(self):
        return _FAKE_CONN[0]


_FAKE_CONN: list = [None]


def _install_fake_pyscard(monkeypatch, answers):
    conn = _FakeConn(answers)
    _FAKE_CONN[0] = conn
    reader = _FakeReader()
    system_mod = types.ModuleType("smartcard.System")
    system_mod.readers = lambda: [reader]
    exceptions_mod = types.ModuleType("smartcard.Exceptions")
    exceptions_mod.NoCardException = type("NoCardException", (Exception,), {})
    exceptions_mod.CardConnectionException = type(
        "CardConnectionException", (Exception,), {})
    sc_mod = types.ModuleType("smartcard")
    sc_mod.__path__ = []
    sc_mod.System = system_mod
    sc_mod.Exceptions = exceptions_mod
    monkeypatch.setitem(sys.modules, "smartcard", sc_mod)
    monkeypatch.setitem(sys.modules, "smartcard.System", system_mod)
    monkeypatch.setitem(sys.modules, "smartcard.Exceptions", exceptions_mod)
    return conn


class TestExecutePath:
    CPLC_VALUE = bytes.fromhex(CPLC_50906420)

    def _answers(self, get_status_sw=(0x69, 0x82)):
        cplc_wire = bytes([0x9F, 0x7F, len(self.CPLC_VALUE)]) + self.CPLC_VALUE
        return {
            SELECT_HEX: (list(bytes.fromhex("6f00")), 0x90, 0x00),
            CPLC_HEX: (list(cplc_wire), 0x90, 0x00),
            GET_STATUS_HEX: ([], *get_status_sw),
        }

    def test_execute_writes_capture_and_stays_read_only(
            self, tmp_path, monkeypatch, capsys):
        _install_fake_pyscard(monkeypatch, self._answers())
        out = tmp_path / "cap.json"
        rc = gpp.main(["--execute", "--reader", "FakeReader",
                       "--json-out", str(out)])
        assert rc == 0
        cap = json.loads(out.read_text())
        names = [e["name"] for e in cap["exchanges"]]
        assert names == ["SELECT_ISD", "GET_DATA_CPLC"]  # GET STATUS opt-in only
        assert cap["exchanges"][0]["command"] == SELECT_HEX

    def test_get_status_opt_in_single_shot_refusal_tolerated(
            self, tmp_path, monkeypatch):
        _install_fake_pyscard(monkeypatch, self._answers())
        out = tmp_path / "cap.json"
        rc = gpp.main(["--execute", "--reader", "FakeReader",
                       "--allow-get-status", "--json-out", str(out)])
        assert rc == 0  # 6982 refusal tolerated, single shot
        cap = json.loads(out.read_text())
        assert [e["name"] for e in cap["exchanges"]] == [
            "SELECT_ISD", "GET_DATA_CPLC", "GET_STATUS_ISD"]

    def test_cplc_identity_printed(self, tmp_path, monkeypatch, capsys):
        _install_fake_pyscard(monkeypatch, self._answers())
        rc = gpp.main(["--execute", "--reader", "FakeReader"])
        assert rc == 0
        assert "50906420/6782" in capsys.readouterr().out

    def test_unreadable_cplc_fails_loudly(self, tmp_path, monkeypatch):
        answers = {SELECT_HEX: ([], 0x90, 0x00),
                   CPLC_HEX: (list(bytes.fromhex("6e00")), 0x6E, 0x00)}
        _install_fake_pyscard(monkeypatch, answers)
        assert gpp.main(["--execute", "--reader", "FakeReader"]) == 1

    def test_no_matching_reader_fails(self, tmp_path, monkeypatch):
        _install_fake_pyscard(monkeypatch, {})
        rc = gpp.main(["--execute", "--reader", "Nowhere"])
        assert rc == 1


class TestDryRunDefault:
    def test_dry_run_never_imports_pyscard(self, monkeypatch, capsys):
        # import trap: sys.modules entry None makes any later
        # `import smartcard…` raise ImportError
        monkeypatch.setitem(sys.modules, "smartcard", None)
        rc = gpp.main([])
        assert rc == 0
        out = capsys.readouterr().out
        assert "DRY RUN" in out and SELECT_HEX.upper() in out

    def test_offline_decoders_never_import_pyscard(self, monkeypatch, capsys):
        monkeypatch.setitem(sys.modules, "smartcard", None)
        assert gpp.main(["--parse-cplc", CPLC_50906420]) == 0
        assert "50906420/6782" in capsys.readouterr().out
        assert gpp.main(["--parse-init-update", _scp02_response()]) == 0
        assert "0433" in capsys.readouterr().out

    def test_execute_without_reader_is_usage_error(self):
        assert gpp.main(["--execute"]) == 2

    def test_bad_hex_is_parse_error(self):
        assert gpp.main(["--parse-cplc", "nothex"]) == 2
