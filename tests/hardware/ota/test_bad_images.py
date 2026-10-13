"""Local unit tests for the bad-image crafting helpers (no hardware).

Runs without NUCULA_OTA_E2E, without pyscard, without a board - part of
the suite self-check (README "Self-check").
"""

import pytest

from .bad_images import (BAD_IMAGE_KINDS, ESP_IMAGE_MAGIC, corrupt_middle,
                         corrupt_tail, craft, is_esp_image, truncated)

MARK = pytest.mark.ota_unit

#: Deterministic stand-in for a firmware image: ESP magic byte + a
#: pseudo-random-looking but reproducible body (LCG, no deps).
def dummy_image(size=64 * 1024):
    body = bytearray([ESP_IMAGE_MAGIC])
    x = 0x1234_5678
    for _ in range(size - 1):
        x = (1103515245 * x + 12345) & 0xFFFF_FFFF
        body.append(x >> 24)
    return bytes(body)


@MARK
def test_truncated_cuts_to_ratio_and_keeps_prefix():
    img = dummy_image(10_000)
    out = truncated(img, ratio=0.60)
    assert len(out) == 6000
    assert out == img[:6000]
    # The ESP magic byte (and the whole header) survives: the failure is
    # the mid-segment end, not a smashed header.
    assert out[0] == ESP_IMAGE_MAGIC


@MARK
def test_truncated_always_keeps_at_least_one_byte():
    out = truncated(b"\xe9\x00", ratio=0.01)
    assert len(out) == 1


@MARK
@pytest.mark.parametrize("bad_ratio", [0, -0.1, 1.0, 2.0])
def test_truncated_rejects_degenerate_ratios(bad_ratio):
    with pytest.raises(ValueError):
        truncated(dummy_image(100), ratio=bad_ratio)


@MARK
def test_truncated_rejects_empty():
    with pytest.raises(ValueError):
        truncated(b"")


@MARK
def test_corrupt_tail_overwrites_only_the_tail():
    img = dummy_image(16 * 1024)
    out = corrupt_tail(img, nbytes=4096, fill=0xFF)
    assert len(out) == len(img)
    assert out[:-4096] == img[:-4096]
    assert out[-4096:] == b"\xff" * 4096
    # header intact
    assert out[0] == ESP_IMAGE_MAGIC


@MARK
def test_corrupt_tail_rejects_whole_image():
    img = dummy_image(100)
    with pytest.raises(ValueError):
        corrupt_tail(img, nbytes=100)
    with pytest.raises(ValueError):
        corrupt_tail(img, nbytes=200)


@MARK
def test_corrupt_middle_flips_only_the_middle():
    img = dummy_image(8192)
    out = corrupt_middle(img, nbytes=64)
    assert len(out) == len(img)
    start = (8192 - 64) // 2
    assert out[:start] == img[:start]
    assert out[start + 64:] == img[start + 64:]
    mid_in = img[start:start + 64]
    mid_out = out[start:start + 64]
    assert mid_in != mid_out
    assert bytes(a ^ b for a, b in zip(mid_in, mid_out)) == b"\xff" * 64
    # double-flip restores (XOR is an involution) - proves determinism
    assert corrupt_middle(out, nbytes=64) == img


@MARK
def test_craft_dispatch_all_kinds_distinct():
    img = dummy_image(32 * 1024)
    outs = {}
    for kind in BAD_IMAGE_KINDS:
        desc, tier = BAD_IMAGE_KINDS[kind]
        assert desc and tier == "tier1"
        outs[kind] = craft(kind, img)
    assert len({id(v) for v in outs.values()}) == len(outs)
    for kind, out in outs.items():
        assert out != img, f"{kind} craft was a no-op"
    # different cases must differ from each other too
    assert len(set(outs.values())) == len(outs)


@MARK
def test_craft_rejects_unknown_kind_and_non_image():
    img = dummy_image(2048)
    with pytest.raises(ValueError, match="unknown bad-image kind"):
        craft("nope", img)
    with pytest.raises(ValueError, match="magic"):
        craft("truncated", b"\x00" + img[1:])


@MARK
def test_is_esp_image_magic_check():
    img = dummy_image(64)
    assert is_esp_image(img)
    assert not is_esp_image(b"\x00" + img[1:])
    assert not is_esp_image(b"")
