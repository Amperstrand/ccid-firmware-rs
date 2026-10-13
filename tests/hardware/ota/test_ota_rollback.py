"""Negative OTA e2e: push crafted BAD images, the board must survive.

Taxonomy (README "Bad-image taxonomy", helpers in bad_images.py):

  truncated          first 60% of the good image
  corrupt-tail       last 4 KiB overwritten with 0xFF
  checksum-invalid   64 bytes bit-flipped mid-body
  never-valid        [case d, opt-in] structurally valid minimal image
                     that never calls esp_ota_mark_app_valid_cancel_rollback

Two defense tiers (README "Research"):

  TIER 1 - device-side rejection. ota.rs calls esp_ota_end() BEFORE
  esp_ota_set_boot_partition(); esp_ota_end runs a full esp_image_verify
  (magic, segment walk, byte checksum, appended SHA-256) on the staged
  bytes. All three crafted kinds break that validation deterministically
  -> the board answers 'E' on the OTA socket and NEVER reboots: the
  running good image keeps serving throughout.

  TIER 2 - bootloader rollback. Only reachable with an image that passes
  esp_image_verify but is not confirmed: esp_ota_set_boot_partition
  writes ESP_OTA_IMG_NEW to otadata, the first boot demotes it to
  ESP_OTA_IMG_PENDING_VERIFY, and unless the app calls
  esp_ota_mark_app_valid_cancel_rollback() the next reset marks the slot
  ESP_OTA_IMG_ABORTED and the bootloader boots the previous good slot
  ("one attempt to boot successfully" - ESP-IDF docs, App Rollback).

Each test asserts the invariant that matters on the bench: the board is
healthy before AND after (within the generous recovery window). If a
crafted image unexpectedly passes tier 1, the test still requires full
recovery - the observed tier goes to the ledger, the assertion on the
'10-minute-recovery' path stays.

Mandatory cleanup: restore_good_image (autouse below) re-pushes
NUCULA_OTA_GOOD_BIN and waits for health after EVERY test in this
module, pass or fail - the bench must never be left on a bad image.
"""

import time

import pytest

from .bad_images import BAD_IMAGE_KINDS
from .conftest import (PROBE_INTERVAL_S, RECOVERY_TIMEOUT_S, board_healthy,
                       ota_port_up, probe, wait_for_healthy)
from .ota_push import push_ex

#: The deterministic tier-1 taxonomy (case d is a separate opt-in test).
CRAFTED_KINDS = ["truncated", "corrupt-tail", "checksum-invalid"]

#: Case (d): the board sits in the never-valid app (no network) until
#: something resets it. A bootlooping image self-triggers; an idling
#: image needs ONE bench power-cycle (nucula: wall adapter). The window
#: below covers reboot + WiFi + vpcd reconnect after that trigger.
NEVERVALID_TRIGGER_WINDOW_S = 300


@pytest.fixture(autouse=True)
def restore_good_image(board, good_bin, ledger, request):
    """MANDATORY cleanup after every negative test: the bench ends on the
    known-good image. Tier-1 rejections never switch slots, so the usual
    path is verify-only; the push path engages whenever health is off."""
    yield
    if board_healthy(board):
        ledger("ota_restore_ok_already_healthy", test=request.node.name)
        return
    ledger("ota_restore_push_start", test=request.node.name)
    for attempt in range(1, 4):
        if ota_port_up(board["ip"]):
            push_ex(board["ip"], good_bin)
        elif attempt == 1:
            # Board dark even for the OTA server: give the boot/rollback
            # path its full window once before re-pushing.
            pass
        if wait_for_healthy(board, timeout_s=RECOVERY_TIMEOUT_S):
            ledger("ota_restore_ok", test=request.node.name, attempts=attempt)
            return
        ledger("ota_restore_retry", test=request.node.name, attempt=attempt)
    pytest.fail(f"restore failed: board {board['ip']} not healthy on the good "
                f"image after 3 attempts - see README 'If rollback fails "
                "entirely' for the USB serial esptool recovery")


@pytest.mark.parametrize("kind", CRAFTED_KINDS)
def test_bad_image_never_takes_the_board_down(kind, craft_bad_image, board,
                                              ledger):
    """Push one crafted bad image. Expected (deterministic): tier-1 'E'
    rejection, no reboot, the good image keeps serving. Belt-and-braces:
    if the board did switch, full recovery within RECOVERY_TIMEOUT_S is
    still required."""
    desc, tier = BAD_IMAGE_KINDS[kind]
    pre = probe(board)
    assert board_healthy(board, pre), (
        f"bench precondition: {board['ip']} not healthy before pushing "
        f"{kind} ({pre}) - fix the bench before running negative tests")

    _, path = craft_bad_image(kind)
    ledger("ota_negative_start", kind=kind, desc=desc, expected_tier=tier,
           image_bytes=path.stat().st_size)

    out = push_ex(board["ip"], path)
    ledger("ota_negative_push", kind=kind, rejected=out["rejected"],
           accepted=out["accepted"], error=out["error"],
           elapsed_s=round(out["elapsed_s"], 2))
    assert out["connected"], f"push did not even connect: {out['error']}"

    if out["rejected"]:
        # TIER 1: board refused at esp_ota_end; it must NOT have rebooted.
        # Short settle, then demand continuous service from the good image.
        time.sleep(PROBE_INTERVAL_S)
        post = probe(board)
        assert board_healthy(board, post), (
            f"{kind}: board rejected the image ('E') but is unhealthy "
            f"afterwards ({post}) - rejection must leave the running "
            "image untouched (ota.rs returns before set_boot_partition)")
        ledger("ota_negative_outcome", kind=kind,
               outcome="tier1_device_rejected", signals=post)
    else:
        # TIER 2 (unexpected for these kinds, but the invariant is what
        # the bench needs): recovery via rollback within the window.
        healthy = wait_for_healthy(board, timeout_s=RECOVERY_TIMEOUT_S)
        assert healthy, (f"{kind}: board ACCEPTED a corrupt image and did "
                         f"not recover within {RECOVERY_TIMEOUT_S}s "
                         f"(last probe: {probe(board)})")
        ledger("ota_negative_outcome", kind=kind,
               outcome="tier2_bootloader_rollback", signals=healthy)


def test_never_valid_image_rolls_back(nevervalid_bin, board, ledger):
    """Case (d) - the scariest: a structurally VALID image that boots and
    idles without ever calling esp_ota_mark_app_valid_cancel_rollback.

    Opt-in only (NUCULA_OTA_NEVERVALID_BIN; building the image is the
    documented follow-up, README 'Case d'). Flow:
      1. push accepted -> board reboots into the never-valid app; the
         OTA server :3232 goes away (a minimal app has no network)
      2. the pending-verify slot must not be confirmed; the NEXT reset
         (self-triggered bootloop, or ONE bench power-cycle for an
         idling image - the test prints when) makes the bootloader mark
         the slot ESP_OTA_IMG_ABORTED and boot the previous good slot
      3. board healthy again = rollback proven end-to-end.
    """
    if not board["rollback_hardened"]:
        pytest.skip(f"--board={board['chip']}: CONFIG_BOOTLOADER_APP_ROLLBACK_"
                    "ENABLE is not set in its tracked sdkconfig (README "
                    "'m5stick caveat') - a never-valid image would just "
                    "keep running with no rollback")

    assert board_healthy(board), "bench precondition: board not healthy"

    out = push_ex(board["ip"], nevervalid_bin)
    ledger("ota_nevervalid_push", rejected=out["rejected"],
           accepted=out["accepted"], error=out["error"])
    assert out["connected"] and out["accepted"] and not out["rejected"], (
        f"never-valid image was not accepted: {out}")

    # Board leaves the good image: wait for the OTA server to disappear
    # (reboot into the bad app). It may also crash-loop straight away.
    deadline = time.time() + 60
    while time.time() < deadline and ota_port_up(board["ip"]):
        time.sleep(PROBE_INTERVAL_S)
    print("\nnever-valid app is running (no :3232). If it idles instead of "
          "boot-looping, POWER-CYCLE the board ONCE now (nucula: wall "
          "adapter) - the pending-verify slot then rolls back on boot.")

    healthy = wait_for_healthy(board, timeout_s=NEVERVALID_TRIGGER_WINDOW_S)
    assert healthy, ("never-valid image did not roll back within "
                     f"{NEVERVALID_TRIGGER_WINDOW_S}s (last probe: "
                     f"{probe(board)}) - manual recovery per README 'If "
                     "rollback fails entirely'")
    ledger("ota_negative_outcome", kind="never-valid",
           outcome="tier2_bootloader_rollback", signals=healthy)
