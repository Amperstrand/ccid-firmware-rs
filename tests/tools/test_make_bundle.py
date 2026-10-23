"""M1 acceptance for tools/hil/make_bundle.py (flash-via-labgrid design §7).

Design acceptance: "two consecutive builds of the same rev produce
identical bundle_id; a dirty tree changes it" — proven at producer
level with fixture binaries (the real-build wiring is M2's bench run).
"""

import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tools" / "hil"))
import make_bundle  # noqa: E402

PARTITIONS_OTA = """\
# Name,   Type, SubType, Offset,  Size,    Flags
nvs,      data, nvs,     0x9000,  0x20000,
phy_init, data, phy,     0x29000, 0x1000,
otadata,  data, ota,     0x30000, 0x2000,
ota_0,    app,  ota_0,   0x40000, 0x3F0000,
factory,  app,  factory, 0x10000, 0x2F0000,
"""

PARTITIONS_FACTORY = """\
# Name,   Type, SubType, Offset,  Size
nvs,      data, nvs,     0x9000,  0x20000,
phy_init, data, phy,     0x29000, 0x1000,
factory,  app,  factory, 0x30000, 0x3F0000,
"""


@pytest.fixture
def workdir(tmp_path):
    (tmp_path / "partitions-ota.csv").write_text(PARTITIONS_OTA)
    (tmp_path / "partitions-factory.csv").write_text(PARTITIONS_FACTORY)
    (tmp_path / "sdkconfig.full").write_text("CONFIG_IDF_TARGET=\"esp32c3\"\n")
    # Valid ESP app image header: magic 0xE9, segment count, flash mode
    # DIO (0x02, issue #309), size/freq byte — then payload.
    (tmp_path / "app.bin").write_bytes(b"\xe9\x03\x02\x02" + b"APP-IMAGE-BYTES" * 64)
    (tmp_path / "bootloader.bin").write_bytes(b"BOOT" * 64)
    (tmp_path / "pt.bin").write_bytes(b"PT" * 64)
    return tmp_path


def run_producer(tmp_path, out, **over):
    base = dict(
        project="esp32-ccid",
        board="c3-nucula-ccid",
        chip="esp32c3",
        layout_source=str(tmp_path / "partitions-ota.csv"),
        layout_id="ota:v1",
        app=str(tmp_path / "app.bin"),
        bootloader=str(tmp_path / "bootloader.bin"),
        partition_table=str(tmp_path / "pt.bin"),
        esp_idf_version="v5.5.5",
        sdkconfig=str(tmp_path / "sdkconfig.full"),
        toolchain="esp/1.98.1.0",
        boot_signature="nucula>",
        baud_cap=115200,
        git_rev="5a6948e" * 5,
        built_at="2026-10-23T01:00:00+00:00",
        out=str(out),
    )
    base.update(over)
    argv = []
    for k, v in base.items():
        flag = "--" + k.replace("_", "-")
        if v is True:
            argv.append(flag)
        elif v is not None:
            argv += [flag, str(v)]
    import io
    import contextlib

    stdout = io.StringIO()
    with contextlib.redirect_stdout(stdout):
        rc = make_bundle.main(argv)
    assert rc == 0
    return stdout.getvalue().strip()


def test_same_inputs_same_bundle_id(workdir, tmp_path):
    """M1 acceptance, first half: two consecutive runs → identical id."""
    id1 = run_producer(workdir, tmp_path / "b1")
    id2 = run_producer(workdir, tmp_path / "b2")
    assert id1 == id2
    assert len(id1) == 64


def test_changed_content_changes_bundle_id(workdir, tmp_path):
    """M1 acceptance, second half (dirty-tree stand-in): any content
    change — here the app bytes — changes the id."""
    id1 = run_producer(workdir, tmp_path / "b1")
    (workdir / "app.bin").write_bytes(b"\xe9\x03\x02\x02" + b"APP-IMAGE-BYTES" * 63 + b"X" * 16)
    id2 = run_producer(workdir, tmp_path / "b2")
    assert id1 != id2


def test_built_at_not_in_identity(workdir, tmp_path):
    """built_at stays in the manifest but out of the hash (spec note)."""
    id1 = run_producer(workdir, tmp_path / "b1", built_at="2026-10-23T01:00:00+00:00")
    id2 = run_producer(workdir, tmp_path / "b2", built_at="2027-01-01T00:00:00+00:00")
    assert id1 == id2
    m1 = json.loads((tmp_path / "b1" / "c3-nucula-ccid" / id1 / "manifest.json").read_text())
    assert m1["built_at"] == "2026-10-23T01:00:00+00:00"


def test_git_dirty_flag_changes_id(workdir, tmp_path):
    id1 = run_producer(workdir, tmp_path / "b1")
    id2 = run_producer(workdir, tmp_path / "b2", git_dirty=True)
    assert id1 != id2


def test_layout_parsed_from_csv(workdir, tmp_path):
    """Offsets come from the CSV (kills hardcoded-offset traps), app slot
    is the FIRST app partition (ota_0 before factory)."""
    run_producer(workdir, tmp_path / "b1")
    m = json.loads(next((tmp_path / "b1" / "c3-nucula-ccid").glob("*/manifest.json")).read_text())
    assert m["layout"]["partitions"]["otadata"]["offset"] == "0x30000"
    assert m["layout"]["partitions"]["ota_0"]["offset"] == "0x40000"
    assert m["layout"]["app_slot"] == "ota_0"
    assert m["targets"]["app"]["offset"] == "0x40000"
    assert m["targets"]["bootloader"]["offset"] == "0x0"
    assert m["targets"]["partition_table"]["offset"] == "0x8000"


def test_factory_layout_picks_factory_slot(workdir, tmp_path):
    id1 = run_producer(workdir, tmp_path / "b1")
    id2 = run_producer(
        workdir, tmp_path / "b2",
        layout_source=str(workdir / "partitions-factory.csv"), layout_id="factory:v1",
    )
    assert id1 != id2
    m = json.loads(next((tmp_path / "b2" / "c3-nucula-ccid").glob("*/manifest.json")).read_text())
    assert m["layout"]["app_slot"] == "factory"
    assert m["targets"]["app"]["offset"] == "0x30000"


def test_binaries_copied_into_bundle_dir(workdir, tmp_path):
    bid = run_producer(workdir, tmp_path / "b1")
    d = tmp_path / "b1" / "c3-nucula-ccid" / bid
    assert (d / "app.bin").read_bytes() == (workdir / "app.bin").read_bytes()
    assert (d / "bootloader.bin").exists() and (d / "pt.bin").exists()
    m = json.loads((d / "manifest.json").read_text())
    assert m["targets"]["app"]["sha256"] == make_bundle.sha256_file(workdir / "app.bin")


def test_manifest_only_mode(workdir, tmp_path):
    bid = run_producer(workdir, tmp_path / "b1", no_copy=True)
    d = tmp_path / "b1" / "c3-nucula-ccid" / bid
    assert (d / "manifest.json").exists()
    assert not (d / "app.bin").exists()


def test_parse_partitions_rejects_appless_csv(tmp_path):
    p = tmp_path / "noapp.csv"
    p.write_text("nvs, data, nvs, 0x9000, 0x1000\n")
    with pytest.raises(SystemExit):
        make_bundle.parse_partitions(p)


def test_parse_partitions_rejects_missing_layout_id(workdir, tmp_path):
    with pytest.raises(SystemExit):
        run_producer(workdir, tmp_path / "b1", layout_id=None)


# ------------------------------------------------ design §4.4 checks 4–7

def test_bootloader_offset_derived_from_chip(workdir, tmp_path):
    """§4.4 check 7: classic xtensa ESP32 → 0x1000, C3 → 0x0 — derived."""
    run_producer(workdir, tmp_path / "c3", chip="esp32c3")
    m = json.loads(next((tmp_path / "c3" / "c3-nucula-ccid").glob("*/manifest.json")).read_text())
    assert m["targets"]["bootloader"]["offset"] == "0x0"
    run_producer(workdir, tmp_path / "esp", chip="esp32", board="m5stick")
    m = json.loads(next((tmp_path / "esp" / "m5stick").glob("*/manifest.json")).read_text())
    assert m["targets"]["bootloader"]["offset"] == "0x1000"


def test_bootloader_offset_contradiction_refused(workdir, tmp_path):
    """The 2026-10-08 m5stick incident made impossible at the producer:
    a C3 offset (0x0) on a classic ESP32 bundle is refused."""
    with pytest.raises(SystemExit, match="check 7"):
        run_producer(workdir, tmp_path / "b1", chip="esp32", bootloader_offset="0x0")


def test_unknown_chip_refused(workdir, tmp_path):
    with pytest.raises(SystemExit, match="no bootloader-offset rule"):
        run_producer(workdir, tmp_path / "b1", chip="esp32pro")


def test_esp32_bundle_requires_bootloader(workdir, tmp_path):
    """§4.4 check 6: no bootloader, no bundle — a bundle is a complete
    reprovision set (the v5.5.1-under-v5.5.5 mixed-generation incident)."""
    with pytest.raises(SystemExit, match="check 6"):
        run_producer(workdir, tmp_path / "b1", bootloader=None)


def test_bootloader_requires_esp_idf_version(workdir, tmp_path):
    """§4.4 check 5: esp_idf_version IS the bootloader-generation identity."""
    with pytest.raises(SystemExit, match="check 5"):
        run_producer(workdir, tmp_path / "b1", esp_idf_version=None)


def test_qio_header_app_refused(workdir, tmp_path):
    """§4.4 check 4 / issue #309: a QIO-header (0x00) app never becomes a
    bundle — it boot-loops every DIO board this repo ships."""
    (workdir / "app.bin").write_bytes(b"\xe9\x03\x00\x02" + b"APP" * 64)
    with pytest.raises(SystemExit, match="309"):
        run_producer(workdir, tmp_path / "b1")


def test_stm32_shape_layout_null(workdir, tmp_path):
    """STM32 bundle (design §4.2): layout null, single app target at
    0x08000000, expected identity via iSerial."""
    bid = run_producer(
        workdir, tmp_path / "b1",
        chip="stm32f469", board="stm32f469",
        layout_source=None, layout_id=None,
        bootloader=None, partition_table=None, usb_iserial="ST2XXX-001",
        sdkconfig=None, esp_idf_version=None,
    )
    m = json.loads((tmp_path / "b1" / "stm32f469" / bid / "manifest.json").read_text())
    assert m["layout"] is None
    assert list(m["targets"]) == ["app"]
    assert int(m["targets"]["app"]["offset"], 0) == 0x08000000
    assert m["expected_identity"]["usb_iserial"] == "ST2XXX-001"
