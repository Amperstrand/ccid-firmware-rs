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
    (tmp_path / "sdkconfig.full").write_text("CONFIG_IDF_TARGET=\"esp32\"\n")
    (tmp_path / "app.bin").write_bytes(b"APP-IMAGE-BYTES" * 64)
    (tmp_path / "bootloader.bin").write_bytes(b"BOOT" * 64)
    (tmp_path / "pt.bin").write_bytes(b"PT" * 64)
    return tmp_path


def run_producer(tmp_path, out, **over):
    base = dict(
        project="esp32-ccid",
        board="c3-nucula-ccid",
        chip="esp32",
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
    (workdir / "app.bin").write_bytes(b"APP-IMAGE-BYTES" * 63 + b"X" * 16)
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
