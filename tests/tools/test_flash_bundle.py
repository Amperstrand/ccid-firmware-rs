"""Refusal-matrix + §4.4 self-check tests for tools/hil/flash_bundle.py.

Fixtures are REAL repo data wherever it exists:
  * the OTA bundles are built from firmware/esp32-ccid/partitions-ota.csv
    (the CSV that produces every esp32-ccid image today);
  * the factory-layout device + CSV come from the 2026-10-20 m5stick
    forensics seed (docs/device-inventory.json), reconstructed as
    tests/tools/fixtures/partitions-micronuts-factory.csv;
  * the golden manifest (tests/tools/fixtures/golden) is a committed
    producer output that must regenerate byte-identically.

The load-bearing class: the 0x30000-otadata/factory-app trap (design
§1.6, §4.5) — build.sh erases "otadata" at a hardcoded 0x30000, which
under the micronuts factory table IS the running factory app. Every
path to that erase is refused here, by construction.
"""

import copy
import json
import shutil
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
HIL = REPO / "tools" / "hil"
sys.path.insert(0, str(HIL))
import flash_bundle  # noqa: E402

FIX = Path(__file__).parent / "fixtures"
REAL_OTA_CSV = REPO / "firmware" / "esp32-ccid" / "partitions-ota.csv"
MICRONUTS_CSV = FIX / "partitions-micronuts-factory.csv"
GOLDEN_DIR = FIX / "golden" / "c3-nucula-ccid" / (
    "59403d0ec5b4f555c7c438b361a8ffac932a9e351c929a53a5840674bbb18a86")
GOLDEN_ID = "59403d0ec5b4f555c7c438b361a8ffac932a9e351c929a53a5840674bbb18a86"

GOLDEN_ARGS = dict(
    project="esp32-ccid", board="c3-nucula-ccid", chip="esp32c3",
    layout_source=str(REAL_OTA_CSV), layout_id="ota:v1",
    app=str(FIX / "golden-src" / "app.bin"),
    bootloader=str(FIX / "golden-src" / "bootloader.bin"),
    partition_table=str(FIX / "golden-src" / "partition-table.bin"),
    esp_idf_version="v5.5.5", sdkconfig=str(FIX / "golden-src" / "sdkconfig.full"),
    toolchain="esp/1.98.1.0", boot_signature="nucula>",
    boot_signature_reject=["i2c: CONFLICT", "CONFLICT! driver_ng is not allowed"],
    baud_cap=115200,
    git_rev="a" * 40, built_at="2026-10-23T00:00:00+00:00",
)


def _argv(d: dict) -> list[str]:
    out = []
    for k, v in d.items():
        flag = "--" + k.replace("_", "-")
        if isinstance(v, list):
            out += [f for item in v for f in (flag, str(item))]
        elif v is True:
            out.append(flag)
        elif v is not None:
            out += [flag, str(v)]
    return out


def build(tmp_path: Path, name: str = "b", **over) -> Path:
    """Produce a bundle via the real CLI seam; returns its dir."""
    args = dict(GOLDEN_ARGS, out=str(tmp_path / name))
    args.update(over)
    rc = flash_bundle.main(["build"] + _argv(args))
    assert rc == 0
    board = args["board"]
    (d,) = (tmp_path / name / board).iterdir()
    return d


def load(bundle: Path) -> dict:
    return json.loads((bundle / "manifest.json").read_text())


def save(bundle: Path, m: dict) -> None:
    (bundle / "manifest.json").write_text(json.dumps(m, indent=2) + "\n")


def retarget(bundle: Path, m: dict) -> dict:
    """Re-record digests after editing target files (so a tampered
    HEADER can be tested without tripping digest-mismatch first)."""
    for t in m["targets"].values():
        p = bundle / t["path"]
        t["sha256"] = flash_bundle.make_bundle.sha256_file(p)
        t["bytes"] = p.stat().st_size
    m["bundle_id"] = flash_bundle.make_bundle.canonical_hash(m)
    return m


@pytest.fixture
def inv() -> dict:
    return json.loads((FIX / "device-inventory.json").read_text())


def verify(bundle: Path, device: str, inventory: dict, **kw):
    return flash_bundle.verify(load(bundle), bundle, device, inventory, **kw)


# ------------------------------------------------------------ the trap

class TestThe0x30000FactoryAppTrap:
    """Design §1.6/§4.5: the incident class, refused by construction."""

    def test_ota_bundle_app_only_onto_factory_device_refused(self, tmp_path, inv):
        """THE exact case: this repo's own OTA table calls 0x30000
        'otadata'; the m5stick (seed forensics) runs 'factory' there.
        An app-only flash is refused on family AND layout grounds —
        never silently erasing the running factory app."""
        b = build(tmp_path)  # otadata@0x30000, family v5.5.5
        v = verify(b, "m5stick", inv, requested_targets={"app"})
        assert v["verdict"] == "refuse"
        assert v["reason"] == "family-mismatch"  # v5.5.1 on device
        assert "bootloader-generation" in v["detail"]

    def test_same_family_factory_layout_still_refused(self, tmp_path, inv):
        """Strip the family difference: layout alone refuses too."""
        inv["devices"]["m5stick"]["family"]["esp_idf_version"] = "v5.5.5"
        b = build(tmp_path)
        v = verify(b, "m5stick", inv, requested_targets={"app"})
        assert v["verdict"] == "refuse"
        assert v["reason"] == "layout-mismatch"
        assert "factory:v1" in v["detail"] and "ota:v1" in v["detail"]

    def test_legacy_hardcoded_erase_refused_by_construction(self, tmp_path, inv):
        """A factory-family bundle (app-only, same family+layout as the
        device — the LEGITIMATE refresh) asked to erase 'otadata' at
        build.sh's hardcoded 0x30000: refused, because 0x30000 is the
        factory APP slot this bundle's own table defines."""
        b = build(tmp_path, layout_source=str(MICRONUTS_CSV), layout_id="factory:v1",
                  board="m5stick", chip="esp32")
        assert load(b)["targets"]["app"]["offset"] == "0x30000"
        inv["devices"]["m5stick"]["family"]["esp_idf_version"] = "v5.5.5"
        v = verify(b, "m5stick", inv, requested_targets={"app"},
                   erase_otadata_at="0x30000")
        assert v["verdict"] == "refuse"
        assert v["reason"] == "otadata-offset-not-in-layout"
        assert "factory-app" in v["detail"]

    def test_erase_overlaps_device_app_slot(self, tmp_path, inv):
        """Defense in depth: layout id and rows match, but the inventory
        records an app slot (a factory remnant — the m5stick-after-
        migration shape) at the otadata offset: an app-only erase that
        would destroy it is refused."""
        b = build(tmp_path)
        inv["devices"]["nucula-c3"]["layout"]["app_partitions"]["factory"] = {
            "offset": "0x30000", "size": "0x2000"}
        v = verify(b, "nucula-c3", inv, requested_targets={"app"},
                   erase_otadata_at="0x30000")
        assert v["verdict"] == "refuse"
        assert v["reason"] == "erase-overlaps-device-app"
        assert "factory" in v["detail"]

    def test_blessed_plans_only_erase_parsed_otadata(self, tmp_path, inv):
        """By construction: every erase in a blessed plan comes from the
        bundle's PARSED layout, never a constant."""
        b = build(tmp_path)
        v = verify(b, "m5stick", inv)  # full bundle = reprovision path
        assert v["verdict"] == "flash"
        otadata = v["plan"]["erases"][0]
        m = load(b)
        assert otadata["offset"] == m["layout"]["partitions"]["otadata"]["offset"]
        assert otadata["offset"] == "0x30000"  # from the CSV, not a default


# ---------------------------------------------------- §4.4 self-checks

class TestSelfChecks:
    def test_check_clean_on_golden(self):
        assert flash_bundle.main(["check", str(GOLDEN_DIR)]) == 0

    def test_check_reparse_of_shipped_csv(self):
        """The golden bundle ships partitions-ota.csv — check 3 re-parses
        it live against the recorded layout."""
        fails = flash_bundle.check_bundle(load(GOLDEN_DIR), GOLDEN_DIR)
        assert fails == []

    def _refused_check(self, tmp_path, mutate, reason, build_post=None):
        b = build(tmp_path)
        if build_post:
            b = build_post(b)
        m = mutate(load(b), b)
        save(b, m)
        fails = flash_bundle.check_bundle(m, b)
        assert fails and fails[0][0] == reason, fails
        v = verify(b, "nucula-c3", json.loads((FIX / "device-inventory.json").read_text()))
        assert v["verdict"] == "refuse" and v["reason"] == reason

    def test_schema_unknown(self, tmp_path, inv):
        b = build(tmp_path)
        m = load(b)
        m["schema"] = "amperstrand.flash-bundle/0"
        save(b, m)
        v = verify(b, "nucula-c3", inv)
        assert v["verdict"] == "refuse" and v["reason"] == "schema-unknown"

    def test_file_missing(self, tmp_path):
        def rm(m, b):
            (b / "app.bin").unlink()
            return m
        self._refused_check(tmp_path, rm, "file-missing")

    def test_missing_digest(self, tmp_path):
        self._refused_check(
            tmp_path, lambda m, b: {**m, "targets": {**m["targets"], "app": {
                **m["targets"]["app"], "sha256": None}}}, "missing-digest")

    def test_digest_mismatch(self, tmp_path):
        def flip(m, b):
            (b / "app.bin").write_bytes(b"\xe9\x03\x02\x02tampered")
            return m
        self._refused_check(tmp_path, flip, "digest-mismatch")

    def test_app_offset_not_in_slot(self, tmp_path):
        self._refused_check(
            tmp_path, lambda m, b: {**m, "targets": {**m["targets"], "app": {
                **m["targets"]["app"], "offset": "0x50000"}}}, "app-offset-not-in-slot")

    def test_layout_csv_mismatch(self, tmp_path):
        def drift(m, b):
            m2 = copy.deepcopy(m)
            m2["layout"]["partitions"]["otadata"]["offset"] = "0x31000"
            return m2

        def with_csv(b):  # ship the CSV so check 3 re-parses live
            shutil.copy(REAL_OTA_CSV, b / REAL_OTA_CSV.name)
            return b
        self._refused_check(tmp_path, drift, "layout-csv-mismatch", build_post=with_csv)

    def test_bad_flash_mode(self, tmp_path):
        def qio(m, b):
            (b / "app.bin").write_bytes(b"\xe9\x03\x00\x02QIO")
            return retarget(b, m)
        self._refused_check(tmp_path, qio, "bad-flash-mode")

    def test_missing_family(self, tmp_path):
        self._refused_check(
            tmp_path, lambda m, b: {**m, "family": {
                **m["family"], "esp_idf_version": None}}, "missing-family")

    def test_missing_bootloader(self, tmp_path):
        self._refused_check(
            tmp_path, lambda m, b: {**m, "targets": {
                k: t for k, t in m["targets"].items() if k != "bootloader"}},
            "missing-bootloader")

    def test_bootloader_offset_mismatch(self, tmp_path):
        """The 2026-10-08 m5stick class: a C3 bundle whose bootloader
        target carries the classic-ESP32 offset."""
        self._refused_check(
            tmp_path, lambda m, b: {**m, "targets": {**m["targets"], "bootloader": {
                **m["targets"]["bootloader"], "offset": "0x1000"}}},
            "bootloader-offset-mismatch")


# -------------------------------------------------------- §4.5 matrix

class TestRefusalMatrix:
    def test_full_bundle_on_unknown_device_flashes_and_seeds(self, tmp_path, inv):
        """§4.5 precedence: the full-bundle row WINS over unknown
        inventory — it IS the first-adoption path."""
        b = build(tmp_path)
        v = verify(b, "ghost-device", inv)
        assert v["verdict"] == "flash"
        assert any("seeds" in w for w in v["warnings"])
        assert v["plan"]["erases"][0]["offset"] == "0x30000"

    def test_app_only_unknown_device_refused(self, tmp_path, inv):
        b = build(tmp_path)
        v = verify(b, "ghost-device", inv, requested_targets={"app"})
        assert v["verdict"] == "refuse" and v["reason"] == "unknown-device"

    def test_app_only_unknown_device_allowed_by_flag(self, tmp_path, inv):
        b = build(tmp_path)
        v = verify(b, "ghost-device", inv, requested_targets={"app"},
                   allow_unknown_inventory=True)
        assert v["verdict"] == "flash"

    def test_app_only_same_bundle_id_idempotent(self, tmp_path, inv):
        b = build(tmp_path)
        inv["devices"]["nucula-c3"]["bundle_id"] = load(b)["bundle_id"]
        v = verify(b, "nucula-c3", inv, requested_targets={"app"})
        assert v["verdict"] == "flash" and v["plan"]["erases"] == []

    def test_app_only_same_family_layout_newer_rev(self, tmp_path, inv):
        b = build(tmp_path)
        v = verify(b, "nucula-c3", inv, requested_targets={"app"})
        assert v["verdict"] == "flash" and v["plan"]["erases"] == []
        assert any("sdkconfig_id unknown" in w for w in v["warnings"])

    def test_app_only_family_mismatch_names_the_generation(self, tmp_path, inv):
        b = build(tmp_path)
        v = verify(b, "m5stick", inv, requested_targets={"app"})
        assert v["reason"] == "family-mismatch"
        assert "v5.5.1" in v["detail"] and "v5.5.5" in v["detail"]

    def test_app_only_sdkconfig_family_mismatch(self, tmp_path, inv):
        b = build(tmp_path)
        inv["devices"]["nucula-c3"]["family"]["sdkconfig_id"] = "f" * 64
        v = verify(b, "nucula-c3", inv, requested_targets={"app"})
        assert v["reason"] == "family-mismatch" and "sdkconfig_id" in v["detail"]

    def test_layout_id_same_but_rows_drifted_is_mismatch(self, tmp_path, inv):
        b = build(tmp_path)
        inv["devices"]["nucula-c3"]["layout"]["app_partitions"]["ota_0"] = {
            "offset": "0x50000", "size": "0x1A0000"}
        v = verify(b, "nucula-c3", inv, requested_targets={"app"})
        assert v["verdict"] == "refuse" and v["reason"] == "layout-mismatch"

    def test_pt_only_refused(self, tmp_path, inv):
        b = build(tmp_path)
        v = verify(b, "nucula-c3", inv, requested_targets={"partition_table"})
        assert v["reason"] == "partial-structural-targets"

    def test_bootloader_only_refused(self, tmp_path, inv):
        b = build(tmp_path)
        v = verify(b, "nucula-c3", inv, requested_targets={"bootloader"})
        assert v["reason"] == "partial-structural-targets"

    def test_bl_plus_pt_without_app_refused(self, tmp_path, inv):
        b = build(tmp_path)
        v = verify(b, "nucula-c3", inv,
                   requested_targets={"bootloader", "partition_table"})
        assert v["reason"] == "partial-structural-targets"

    def test_unknown_requested_target_refused(self, tmp_path, inv):
        b = build(tmp_path)
        v = verify(b, "nucula-c3", inv, requested_targets={"app", "nvs"})
        assert v["reason"] == "partial-structural-targets"

    def test_validated_erase_in_app_only_plan(self, tmp_path, inv):
        b = build(tmp_path)
        v = verify(b, "nucula-c3", inv, requested_targets={"app"},
                   erase_otadata_at="0x30000")
        assert v["verdict"] == "flash"
        assert v["plan"]["erases"][0]["offset"] == "0x30000"

    def test_erase_at_wrong_offset_refused(self, tmp_path, inv):
        b = build(tmp_path)
        v = verify(b, "nucula-c3", inv, requested_targets={"app"},
                   erase_otadata_at="0x31000")
        assert v["reason"] == "otadata-offset-not-in-layout"


# ------------------------------------------------------------- plans

class TestBlessedPlan:
    def test_images_descending_offsets(self, tmp_path, inv):
        b = build(tmp_path)
        v = verify(b, "nucula-c3", inv)
        offs = [int(i["offset"], 0) for i in v["plan"]["images"]]
        assert offs == sorted(offs, reverse=True)
        assert v["plan"]["boot_signature"] == "nucula>"
        assert v["plan"]["baud_cap"] == 115200

    def test_classic_esp32_bootloader_at_0x1000(self, tmp_path, inv):
        b = build(tmp_path, board="m5stick", chip="esp32")
        v = verify(b, "m5stick", inv)
        assert v["verdict"] == "flash"
        bl = next(i for i in v["plan"]["images"] if i["offset"] == "0x1000")
        assert bl["path"].endswith("bootloader.bin")

    def test_stm32_single_app_bundle(self, tmp_path, inv):
        b = build(tmp_path, board="stm32f469", chip="stm32f469",
                  layout_source=None, layout_id=None, bootloader=None,
                  partition_table=None, sdkconfig=None, esp_idf_version=None,
                  usb_iserial="ST2XXX-001")
        v = verify(b, "stm32f469", inv)
        assert v["verdict"] == "flash"
        assert [i["offset"] for i in v["plan"]["images"]] == ["0x8000000"]


# ---------------------------------------------------------- CLI + golden

class TestCliAndGolden:
    def test_golden_regenerates_byte_identically(self, tmp_path):
        b = build(tmp_path)
        assert (b / "manifest.json").read_bytes() == \
            (GOLDEN_DIR / "manifest.json").read_bytes()
        assert load(b)["bundle_id"] == GOLDEN_ID

    def test_golden_verify_via_cli(self, tmp_path, capsys):
        rc = flash_bundle.main([
            "verify", str(GOLDEN_DIR), "--device", "nucula-c3",
            "--inventory", str(FIX / "device-inventory.json"),
            "--targets", "app", "--json"])
        assert rc == 0
        v = json.loads(capsys.readouterr().out)
        assert v["verdict"] == "flash"

    def test_cli_refuse_exit_code(self, tmp_path, capsys):
        rc = flash_bundle.main([
            "verify", str(GOLDEN_DIR), "--device", "m5stick",
            "--inventory", str(FIX / "device-inventory.json"),
            "--targets", "app"])
        assert rc == 1
        assert "REFUSE family-mismatch" in capsys.readouterr().out

    def test_cli_check_exit_code(self, tmp_path, capsys):
        b = build(tmp_path)
        (b / "app.bin").unlink()
        assert flash_bundle.main(["check", str(b)]) == 1
        assert "file-missing" in capsys.readouterr().out

    def test_inventory_schema_guard(self, tmp_path, capsys):
        bad = tmp_path / "inv.json"
        bad.write_text(json.dumps({"schema": "other/1", "devices": {}}))
        rc = flash_bundle.main([
            "verify", str(GOLDEN_DIR), "--device", "x", "--inventory", str(bad)])
        assert rc == 1
