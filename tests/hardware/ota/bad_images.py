"""Bad-image crafting helpers for the OTA rollback suite.

Pure functions over bytes - unit-testable without any hardware
(test_bad_images.py). Each helper derives from a VALID image and produces
one taxonomy case. The expected failure tier for every case here is
TIER 1 (device-side rejection): firmware/esp32-ccid/src/ota.rs calls
esp_ota_end() before esp_ota_set_boot_partition(), and esp_ota_end runs a
full esp_image_verify (magic byte, segment walk, byte checksum, appended
SHA-256) on the staged bytes - so any in-place corruption of a real
image is rejected with 'E' and the running image is never touched.

The bootloader-level TIER 2 rollback (pending-verify state machine) is
only reachable with an image that PASSES esp_image_verify but never
calls esp_ota_mark_app_valid_cancel_rollback() - see README.md
"taxonomy case d" for why that needs its own build and is a follow-up.

ESP image format reference (esp_image_format.h / esptool):
  byte 0   magic 0xE9
  byte 1   segment count
  byte 2-3 flash mode/size+freq flags
  byte 4-7 entry point
  ...      8-byte segment headers (load addr + len) + segment data
  ...      padding, then a simple byte checksum (sum of data bytes,
           complemented), then (esp-idf default) the full-image SHA-256.
The checksum and the SHA-256 live at the END of the image, which is why
"corrupted-tail" destroys them outright and "checksum-invalid" (mid-body
byte flips) breaks both the byte checksum and the appended SHA-256.
"""

ESP_IMAGE_MAGIC = 0xE9

#: Taxonomy table: id -> (one-line description, expected tier).
#  Tier semantics documented in README.md.
BAD_IMAGE_KINDS = {
    "truncated": ("first 60% of a valid image (interrupted-transfer shape)", "tier1"),
    "corrupt-tail": ("valid image, last 4 KiB overwritten with 0xFF "
                     "(never-written-flash shape)", "tier1"),
    "checksum-invalid": ("valid image, 64 bytes bit-flipped mid-body "
                         "(in-flight corruption shape)", "tier1"),
}


def is_esp_image(data):
    """True when the buffer starts with the ESP app magic byte 0xE9."""
    return bool(data) and data[0] == ESP_IMAGE_MAGIC


def truncated(data, ratio=0.60):
    """First `ratio` of the image. Announced-size protocol means the board
    sees a complete stream of short length; esp_image_verify fails inside
    the segment walk (image ends mid-segment) -> 'E'."""
    if not data:
        raise ValueError("cannot truncate an empty image")
    if not 0.0 < ratio < 1.0:
        raise ValueError(f"ratio must be in (0, 1), got {ratio}")
    return data[: max(1, int(len(data) * ratio))]


def corrupt_tail(data, nbytes=4096, fill=0xFF):
    """Overwrite the last `nbytes` with `fill` (default 0xFF = erased
    flash). Destroys the appended SHA-256 and the byte checksum that
    live at the image tail -> esp_ota_end validation failure -> 'E'."""
    if not data:
        raise ValueError("cannot corrupt an empty image")
    if nbytes <= 0:
        raise ValueError("nbytes must be positive")
    if nbytes >= len(data):
        raise ValueError(f"nbytes {nbytes} >= image size {len(data)} "
                         "- that is truncation, not tail corruption")
    out = bytearray(data)
    out[-nbytes:] = bytes([fill]) * nbytes
    return bytes(out)


def corrupt_middle(data, nbytes=64, mask=0xFF):
    """XOR (bit-flip) `nbytes` starting at the geometric middle. Length,
    magic and segment headers survive - only body bytes and therefore the
    byte checksum + appended SHA-256 break -> 'E'. Deterministic: no RNG."""
    if not data:
        raise ValueError("cannot corrupt an empty image")
    if nbytes <= 0:
        raise ValueError("nbytes must be positive")
    if nbytes >= len(data):
        raise ValueError(f"nbytes {nbytes} >= image size {len(data)}")
    start = (len(data) - nbytes) // 2
    out = bytearray(data)
    for i in range(start, start + nbytes):
        out[i] ^= mask
    return bytes(out)


def craft(kind, data):
    """Dispatch on a taxonomy id from BAD_IMAGE_KINDS. Raises ValueError
    on an unknown kind (typo guard for the parameterized tests)."""
    if kind not in BAD_IMAGE_KINDS:
        raise ValueError(f"unknown bad-image kind {kind!r}; "
                         f"known: {sorted(BAD_IMAGE_KINDS)}")
    if not is_esp_image(data):
        # Not fatal for crafting, but every real bench image starts with
        # 0xE9; hitting this in the e2e suite means the GOOD image env
        # var points at something that is not an esp-idf app image.
        raise ValueError("source image does not start with the ESP "
                         f"app magic byte 0x{ESP_IMAGE_MAGIC:02X}")
    if kind == "truncated":
        return truncated(data)
    if kind == "corrupt-tail":
        return corrupt_tail(data)
    if kind == "checksum-invalid":
        return corrupt_middle(data)
    raise AssertionError("unreachable")  # pragma: no cover
