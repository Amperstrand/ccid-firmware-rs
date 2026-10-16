"""ACR1252 emulation HIL tests (gated behind --acr-emu — see module doc).

Run: pytest tests/hardware/labgrid/test_acr1252_emu_hil.py -v --hil --acr-emu

Tier 1 (control plane, no RF needed) proves the escape transport:
  - helper installs, pcscd toggles cleanly under the bench flock,
    enter/exit exchanges complete.
Tier 2 (RF observability) asserts bench readers actually SEE the
emulated tag — ENABLED ONLY after the coupling verification documented
in acr1252_emu.py STATUS (first bench run 2026-10-16: not yet observed).
"""

import pytest

pytest.importorskip("smartcard", reason="bench host only")

from tests.hardware.labgrid import acr1252_emu  # noqa: E402


def pytest_addoption(parser):
    parser.addoption("--acr-emu", action="store_true", default=False,
                     help="enable ACR1252 card-emulation HIL tests")


def _acr_enabled(request):
    return request.config.getoption("--acr-emu", False)


@pytest.fixture()
def bench(request):
    return request.getfixturevalue("bench") if "bench" in request.fixturenames else None


def test_helper_install_and_control_plane(request, bench):
    if not _acr_enabled(request):
        pytest.skip("ACR1252 emulation tier-1 needs --acr-emu")
    acr1252_emu.install_helper(bench)
    out = None
    # enter/exit under pcscd pause, exactly like the context manager
    acr1252_emu._pcscd(bench, "stop")
    try:
        out = acr1252_emu._run(bench, "enter")
    finally:
        acr1252_emu._pcscd(bench, "start")
    assert out in ("OK", "SENT-NO-REPLY")  # reply-drop is documented firmware behavior


def test_emu_context_roundtrip(request, bench):
    if not _acr_enabled(request):
        pytest.skip("ACR1252 emulation tier-1 needs --acr-emu")
    with acr1252_emu.Acr1252Emu(bench, ndef_uri="https://bench.test/acr-emu"):
        assert True  # entering without raising is the tier-1 contract
    # pcscd must be healthy after the context exits
    r = bench.run("python3 -c 'from smartcard.System import readers; print(len(readers()))'")
    assert r.returncode == 0 and int(r.stdout.strip() or 0) >= 1


@pytest.mark.xfail(reason="tier-2: emulated tag not yet observed on bench readers "
                          "(RF coupling unverified 2026-10-16) — see acr1252_emu.py STATUS",
                   strict=False)
def test_nucula_vpcd_sees_emulated_tag(request, bench):
    if not _acr_enabled(request):
        pytest.skip("ACR1252 emulation tier-2 needs --acr-emu")
    with acr1252_emu.Acr1252Emu(bench, ndef_uri="https://bench.test/acr-emu"):
        r = bench.run(
            "python3 -c '\n"
            "from smartcard.System import readers\n"
            "rs=[str(x) for x in readers() if \"Nucula\" in str(x)]\n"
            "print(rs)'")
        assert "Nucula VPCD" in r.stdout
        # presence check via SCardGetStatusChange is the tier-2 assert
        raise AssertionError("tier-2 assert not implemented until coupling verified")
