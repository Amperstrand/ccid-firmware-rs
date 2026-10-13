"""Positive OTA e2e: push the KNOWN-GOOD image, board must come back
healthy on every recovery signal (README "Recovery signals").

Requires: NUCULA_OTA_E2E=1, NUCULA_OTA_GOOD_BIN=<good image>, --board.
Never runs otherwise. Not executed during authoring (bench busy) - this
is the bench-facing half of the suite.
"""

import pytest

from .conftest import RECOVERY_TIMEOUT_S, probe, wait_for_healthy
from .ota_push import push_ex


def test_good_image_push_and_board_returns(board, good_bin, ledger, require_ping):
    """Push the good image; within RECOVERY_TIMEOUT_S the board must:
    answer ICMP ping (optional strictness), serve :3232 again, hold a
    vpcd ESTAB session, and have its reader slot answer pcscd (ATR on
    the nucula's SmartPGP slot; empty-reader answer is fine on m5stick).

    This is also the reference flow the rollback tests recover to.
    """
    pre = probe(board)
    ledger("ota_positive_start", signals=pre, image=good_bin)
    assert pre["ota_port"], (f"OTA server on {board['ip']}:3232 not answering "
                             "before the push - bench precondition failed")

    out = push_ex(board["ip"], good_bin)
    ledger("ota_positive_push", rejected=out["rejected"],
           accepted=out["accepted"], sent=out["sent"], total=out["total"],
           error=out["error"], elapsed_s=round(out["elapsed_s"], 2))
    # A good image must survive esp_ota_end: a rejection here means the
    # GOOD_BIN is not a valid image for this board (wrong build/target).
    assert out["connected"], f"push did not connect: {out['error']}"
    assert not out["rejected"], ("board rejected the supposedly good image "
                                 f"('E') - check NUCULA_OTA_GOOD_BIN: {good_bin}")

    healthy = wait_for_healthy(board, timeout_s=RECOVERY_TIMEOUT_S)
    assert healthy, ("board did not come back healthy within "
                     f"{RECOVERY_TIMEOUT_S}s after a GOOD image push "
                     f"(last probe: {probe(board)})")

    if require_ping:
        assert healthy["ping"], ("board serves TCP but does not answer "
                                 "ping - soften with NUCULA_OTA_REQUIRE_PING=0 "
                                 "if this bench filters ICMP")
    if board["expect_card"]:
        assert healthy["atr_hex"], ("reader slot answered but no ATR - "
                                    "SmartPGP card not coupling (runbook "
                                    "'Power': wall adapter for stable RF)")

    ledger("ota_positive_pass", signals=healthy)
