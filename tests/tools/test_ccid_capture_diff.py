"""Determinism + semantics tests for host-tools/ccid_capture_diff.py (#143).

Pure stdlib+pytest (python-collect minimal env). The committed sample
captures under fixtures/captures/ are SYNTHETIC (tagged as such) — they
document the format and exercise the mask semantics; no bench evidence
lives here.
"""

import copy
import json
from pathlib import Path
from sys import path as sys_path

import pytest

REPO = Path(__file__).resolve().parents[2]
sys_path.insert(0, str(REPO / "host-tools"))
import ccid_capture_diff as ccd  # noqa: E402

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "captures"
SAMPLE_A = FIXTURES / "reader-a-sample.json"
SAMPLE_B = FIXTURES / "reader-b-sample.json"


def load(path):
    return ccd.load_capture(Path(path))


def test_sample_pair_is_equal_modulo_masks(tmp_path, capsys):
    rc = ccd.main([str(SAMPLE_A), str(SAMPLE_B)])
    out = capsys.readouterr().out
    assert rc == 0
    assert "EQUAL" in out
    # the masked fields are reported, not differenced
    assert "INIT_UPDATE.command: equal modulo 8 masked" in out
    assert "INIT_UPDATE.response: equal modulo 16 masked" in out


def test_strict_mode_flags_the_masked_bytes(tmp_path, capsys):
    rc = ccd.main([str(SAMPLE_A), str(SAMPLE_B), "--strict"])
    out = capsys.readouterr().out
    assert rc == 1
    assert "deterministic byte(s) differ" in out


def test_json_output_shape(tmp_path, capsys):
    rc = ccd.main([str(SAMPLE_A), str(SAMPLE_B), "--json"])
    payload = json.loads(capsys.readouterr().out)
    assert rc == 0
    assert payload["equal"] is True
    assert payload["a"]["reader"].startswith("Cherry")
    assert payload["b"]["reader"].startswith("ACR")


def test_deterministic_field_diff_reports_offset(tmp_path):
    a = load(SAMPLE_A)
    b = copy.deepcopy(a)
    # CPLC byte 4 (IC fabricator date, deterministic) differs
    raw = bytearray.fromhex(
        json.loads(SAMPLE_B.read_text())["exchanges"][1]["response"]
    )
    raw[4] ^= 0xFF
    b["ex"]["GET_DATA_CPLC"]["response"] = bytes(raw)
    lines, diffs = ccd.diff_captures(a, b)
    assert diffs >= 1
    assert any("GET_DATA_CPLC.response" in ln and "deterministic byte" in ln for ln in lines)
    assert any("[  4]" in ln for ln in lines)


def test_missing_exchange_is_a_diff():
    a = load(SAMPLE_A)
    b = copy.deepcopy(a)
    del b["ex"]["SELECT_ISD"]
    lines, diffs = ccd.diff_captures(a, b)
    assert diffs == 1
    assert any(
        ln.startswith("DIFF SELECT_ISD: exchange only in capture A") for ln in lines
    )


def test_length_change_is_a_diff():
    a = load(SAMPLE_A)
    b = copy.deepcopy(a)
    b["ex"]["SELECT_ISD"]["response"] = a["ex"]["SELECT_ISD"]["response"][:-1]
    lines, diffs = ccd.diff_captures(a, b)
    assert diffs == 1
    assert any(ln.startswith("DIFF SELECT_ISD.response: length") for ln in lines)


def test_diff_output_is_order_independent():
    a, b = load(SAMPLE_A), load(SAMPLE_B)
    lines_ab, diffs_ab = ccd.diff_captures(a, b)
    lines_ba, diffs_ba = ccd.diff_captures(b, a)
    assert diffs_ab == diffs_ba
    assert lines_ab == lines_ba  # name-sorted => byte-identical report


def test_mask_declared_in_only_one_capture_still_masks():
    # union semantics: A declares the mask, B does not — the mask holds
    # (the reader that DID capture the field list knew it was random).
    a = load(SAMPLE_A)
    b = copy.deepcopy(a)
    b["ex"]["INIT_UPDATE"]["masks"] = {"command": [], "response": []}
    lines, diffs = ccd.diff_captures(a, b)
    assert diffs == 0


def test_mask_past_buffer_is_capture_error(tmp_path):
    bad = json.loads(SAMPLE_A.read_text())
    bad["exchanges"][2]["random_fields"][0]["length"] = 64
    path = tmp_path / "bad.json"
    path.write_text(json.dumps(bad))
    with pytest.raises(ccd.CaptureError, match="exceeds command length"):
        ccd.load_capture(path)


def test_bad_hex_is_capture_error(tmp_path):
    bad = json.loads(SAMPLE_A.read_text())
    bad["exchanges"][0]["response"] = "zz00"
    path = tmp_path / "bad.json"
    path.write_text(json.dumps(bad))
    with pytest.raises(ccd.CaptureError, match="not hex"):
        ccd.load_capture(path)


def test_duplicate_exchange_name_is_capture_error(tmp_path):
    bad = json.loads(SAMPLE_A.read_text())
    bad["exchanges"].append(copy.deepcopy(bad["exchanges"][0]))
    path = tmp_path / "bad.json"
    path.write_text(json.dumps(bad))
    with pytest.raises(ccd.CaptureError, match="duplicate"):
        ccd.load_capture(path)


def test_overlapping_masks_are_unioned():
    ex = {
        "name": "X",
        "command": "00" * 8,
        "response": "00" * 8,
        "random_fields": [
            {"field": "f1", "side": "response", "offset": 0, "length": 3},
            {"field": "f2", "side": "response", "offset": 2, "length": 3},
            {"field": "f3", "side": "response", "offset": 7, "length": 1},
        ],
    }
    spans = ccd.mask_spans("X", ex)
    assert spans["response"] == [(0, 5), (7, 8)]


def test_cli_capture_error_exit_code(tmp_path, capsys):
    bad = tmp_path / "bad.json"
    bad.write_text("{}")
    assert ccd.main([str(SAMPLE_A), str(bad)]) == 2
