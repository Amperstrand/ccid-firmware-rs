"""Local unit tests for the OTA rollback wiring map (no hardware).

Static coherence locks for the WS2 rollback wiring (t_a11d2275):

1. Every build.sh board that serves OTA via a wifi main
   (``wifi-ccid`` / ``wifi-vpcd`` features) while building on a
   ``CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`` sdkconfig MUST have a
   main that reaches ``mark_app_valid`` — a good image pushed over the
   OTA seat must confirm itself, or it reverts on the next reset.
2. The OTA suite's ``conftest.py`` ``rollback_hardened`` flags must
   match the knob state of each bench board's tracked sdkconfig
   (nucula -> sdkconfig.full, m5stick -> sdkconfig-xtensa-vpcd.full).
3. The m5stick knob-unset exemption stays documented (README
   "m5stick caveat": flipping it means reflashing a bootloader — a
   bench decision, not a config edit).

Runs without NUCULA_OTA_E2E, without pyscard, without a board - part of
the suite self-check (README "Self-check"). Stdlib only.
"""

import re
from pathlib import Path

import pytest

MARK = pytest.mark.ota_unit

#: Repo root (tests/hardware/ota/test_wiring_coherence.py -> root).
ROOT = Path(__file__).resolve().parents[3]
ESP32_DIR = ROOT / "firmware" / "esp32-ccid"

#: feature -> the main source that runs when it is selected.
WIFI_MAIN_SOURCES = {
    "wifi-ccid": "src/wifi_ccid.rs",
    "wifi-vpcd": "src/wifi_vpcd.rs",
}

#: conftest.py bench board -> the build.sh case that builds its bench
#: image (docs/VPCD_WIFI_RUNBOOK.md "Firmware build + flash").
BENCH_BOARD_CASE = {
    "nucula": "wifi-vpcd",
    "m5stick": "m5stick-vpcd",
}


def _parse_build_cases() -> dict:
    """board case label -> {sdkconfig, features} from build.sh.

    Splits the case dispatch on ``;;`` and pulls SDKCONFIG=/FEATURE_ARGS=
    per block; ``${BOARD}`` inside the feature list resolves to the case
    label (the wifi-ccid/wifi-vpcd entries use that indirection).
    """
    text = (ESP32_DIR / "build.sh").read_text()
    cases = {}
    for chunk in text.split(";;"):
        label = re.search(r"^    ([a-z0-9+=-]+)\)$", chunk, re.MULTILINE)
        sdk = re.search(r'SDKCONFIG="\$\{SCRIPT_DIR\}/([^"]+)"', chunk)
        feats = re.search(r"--features\s+\"?([^)\"]+)\"?", chunk)
        if not (label and sdk and feats):
            continue
        name = label.group(1)
        feature_list = feats.group(1).replace("${BOARD}", name)
        cases[name] = {
            "sdkconfig": sdk.group(1),
            "features": [f.strip() for f in feature_list.split(",")],
        }
    return cases


def _knob_set(sdkconfig_name: str) -> bool:
    """True iff the tracked sdkconfig sets the bootloader rollback knob."""
    text = (ESP32_DIR / sdkconfig_name).read_text()
    return re.search(r"^CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y$", text, re.MULTILINE) is not None


def _main_reaches_mark(main_source: str) -> bool:
    """True iff the main source reaches an OTA self-confirmation path.

    Post-#461 doctrine: serving mains confirm SERVE-PROVEN — they arm the
    gate (ota_validity::arm) and the ccid kernel marks valid on the first
    completed exchange; bring-up-proven mains mark via
    station_bring_up/mark_app_valid. Either path satisfies coherence.
    """
    text = (ESP32_DIR / main_source).read_text()
    return bool(
        re.search(r"station_bring_up\(|mark_app_valid\(|ota_validity::arm\(", text)
    )


@MARK
def test_wifi_serving_mains_confirm_when_knob_set():
    """Knob-SET config + wifi serving main => the main must mark valid.

    This is the WS2 coherence rule: sdkconfig.full enables the pending-
    verify state machine (baked into the bootloader), so any wifi main
    built on it that serves OTA must confirm itself after bring-up —
    exactly one definition point: wifi_common::mark_app_valid
    (directly, or via station_bring_up).
    """
    cases = _parse_build_cases()
    offenders = []
    for board, info in sorted(cases.items()):
        wifi_features = [f for f in info["features"] if f in WIFI_MAIN_SOURCES]
        if not wifi_features:
            continue
        if not _knob_set(info["sdkconfig"]):
            continue  # knob-unset pair: documented m5stick exemption below
        for feature in wifi_features:
            main = WIFI_MAIN_SOURCES[feature]
            if not _main_reaches_mark(main):
                offenders.append(
                    f"build.sh '{board}': feature '{feature}' builds on "
                    f"{info['sdkconfig']} (CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y) "
                    f"but {main} never reaches ota_validity::arm/mark_app_valid/station_bring_up "
                    f"— a good image pushed over the OTA seat would revert "
                    f"on the next reset"
                )
    assert not offenders, "\n".join(offenders)


@MARK
def test_conftest_hardened_flags_match_tracked_configs():
    """conftest.py rollback_hardened must mirror the sdkconfig knob state.

    rollback_hardened gates the tier-2 never-valid test per board; if a
    tracked sdkconfig knob and this flag drift apart, the suite either
    skips a test that would pass or runs one that cannot.
    """
    conf = (ROOT / "tests" / "hardware" / "ota" / "conftest.py").read_text()
    cases = _parse_build_cases()
    for bench_board, case_name in sorted(BENCH_BOARD_CASE.items()):
        flag = re.search(
            rf'"{bench_board}":.*?"rollback_hardened": (True|False)', conf, re.DOTALL
        )
        assert flag, f"conftest.py: no rollback_hardened flag for {bench_board}"
        knob = _knob_set(cases[case_name]["sdkconfig"])
        assert flag.group(1) == str(knob), (
            f"conftest.py {bench_board}: rollback_hardened={flag.group(1)} "
            f"but build.sh '{case_name}' uses "
            f"{cases[case_name]['sdkconfig']} with knob={'y' if knob else 'not set'}"
        )


@MARK
def test_m5stick_knob_exemption_stays_documented():
    """m5stick-vpcd (mark-valid caller, knob UNSET) is a deliberate pair.

    The README "m5stick caveat" records the owner decision: enabling the
    knob means flashing a new bootloader (bench decision), not editing
    the sdkconfig. If this test fails, the caveat prose was dropped or
    renamed - restore it before touching any config.
    """
    readme = (ROOT / "tests" / "hardware" / "ota" / "README.md").read_text()
    assert "m5stick caveat" in readme
    cases = _parse_build_cases()
    m5stick = cases["m5stick-vpcd"]
    assert "wifi-vpcd" in m5stick["features"]
    assert not _knob_set(m5stick["sdkconfig"]), (
        "sdkconfig-xtensa-vpcd.full flipped the rollback knob on - that "
        "is a bootloader-reflash bench decision (README 'm5stick caveat'); "
        "update conftest.py rollback_hardened + the caveat together"
    )
