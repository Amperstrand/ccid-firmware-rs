"""Host port of libccid's parse.c CCID-class-descriptor checker (issue #143).

Reference: ``reference/CCID/src/parse.c`` (upstream LudovicRousseau/CCID,
submodule @ 1a0f75d, CCID 1.7.1-8) — ``ccid_parse_interface_descriptor()``
(L290-650). parse.c itself needs a live USB device (libusb string/control
transfers); this module ports the STATIC checks that only need the 54-byte
class-descriptor bytes, so they can run in CI against descriptor fixtures
generated from our device-profile consts (``tests/tools/fixtures/ccid/*.hex``
— produced by ``device_profile.rs::descriptor_fixtures_match_profile_consts``,
never hand-edited).

Faithfulness rules (reference-first doctrine):

* The byte layout is parse.c's, NOT re-derived from the CCID PDF:
  ``device_descriptor[0]``/``[1]`` are bLength/bDescriptorType, fields run
  from bcdCCID at [2] — i.e. the descriptor INCLUDES its 2-byte header
  (parse.c L417-436).
* The hard failures are exactly parse.c's two ``return true`` bail-outs on
  the class descriptor (bad bLength L418-422, bad bDescriptorType L425-434)
  plus the field-decode tables transcribed verbatim (dwFeatures bit names
  L587-628, voltage L439-444, protocols L448-451, PIN L643-646, ...).
* The GET_CLOCK_FREQUENCIES / GET_DATA_RATES probe legs (parse.c L458-509,
  L515-562) are live-control-transfer checks — out of scope for a static
  fixture; ``bNumClockSupported``/``bNumDataRatesSupported`` decoding is
  still ported.

Differences from parse.c are marked ``# PORT NOTE:`` with the reason.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

# parse.c L418: `if (device_descriptor[0] != 0x36)` -> UNSUPPORTED bLength
CCID_CLASS_DESCRIPTOR_LENGTH = 0x36  # 54 bytes incl. the 2-byte header
# parse.c L425: `if (device_descriptor[1] != 0x21)` -> UNSUPPORTED bDescriptorType
CCID_CLASS_DESCRIPTOR_TYPE = 0x21

FIXTURE_DIR = Path(__file__).resolve().parent / "fixtures" / "ccid"

# Fixture name -> (vid, pid) of the shipping profile (device_profile.rs;
# cross-checked by the Rust freshness lock, not re-asserted here).
FIXTURE_PROFILES = {
    "cherry-smartterminal-st2xxx": (0x046A, 0x003E),
    "gemalto-idbridge-ct30": (0x08E6, 0x3437),
    "gemalto-idbridge-k30": (0x08E6, 0x3438),
}


def _dw2i(data: bytes, offset: int) -> int:
    """parse.c dw2i(): little-endian u32 at byte offset."""
    return int.from_bytes(data[offset : offset + 4], "little")


@dataclass
class CcidDescriptorReport:
    """Decode of one 54-byte CCID class descriptor + parse.c findings."""

    b_length: int = 0
    b_descriptor_type: int = 0
    bcd_ccid: int = 0
    b_max_slot_index: int = 0
    b_voltage_support: int = 0
    dw_protocols: int = 0
    dw_default_clock: int = 0
    dw_maximum_clock: int = 0
    b_num_clock_supported: int = 0
    dw_data_rate: int = 0
    dw_max_data_rate: int = 0
    b_num_data_rates_supported: int = 0
    dw_max_ifsd: int = 0
    dw_synch_protocols: int = 0
    dw_mechanical: int = 0
    dw_features: int = 0
    dw_max_ccid_message_length: int = 0
    b_class_get_response: int = 0
    b_class_envelope: int = 0
    w_lcd_layout: int = 0
    b_pin_support: int = 0
    b_max_ccid_busy_slots: int = 0
    # parse.c findings: "errors" are the checker's bail-out class (bad
    # bLength/bDescriptorType); "notes" are the decode annotations it
    # prints per field (kept so the report mirrors the reference output).
    errors: list[str] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)


# dwFeatures decode table — transcribed from parse.c L587-628.
FEATURE_BITS: dict[int, str] = {
    0x00000002: "Automatic parameter configuration based on ATR data",
    0x00000004: "Automatic activation of ICC on inserting",
    0x00000008: "Automatic ICC voltage selection",
    0x00000010: "Automatic ICC clock frequency change according to parameters",
    0x00000020: "Automatic baud rate change according to frequency and Fi, Di params",
    0x00000040: "Automatic parameters negotiation made by the CCID",
    0x00000080: "Automatic PPS made by the CCID",
    0x00000100: "CCID can set ICC in clock stop mode",
    0x00000200: "NAD value other than 00 accepted (T=1)",
    0x00000400: "Automatic IFSD exchange as first exchange (T=1)",
    0x00000800: "ICCD token",
    0x00001000: "USB Wake up signaling supported on card insertion and removal",
}
# parse.c L609-626: exchange level = dwFeatures bits 16..18 & 0x07.
EXCHANGE_LEVELS: dict[int, str] = {
    0x00: "Character level exchange",
    0x01: "TPDU level exchange",
    0x02: "Short APDU level exchange",
    0x04: "Short and Extended APDU level exchange",
}


def check_ccid_class_descriptor(data: bytes) -> CcidDescriptorReport:
    """Port of the static part of parse.c ccid_parse_interface_descriptor().

    ``data`` is the FULL class descriptor INCLUDING the 2-byte header
    (bLength, bDescriptorType), exactly as ``get_ccid_device_descriptor()``
    hands it to parse.c (L405-447).
    """
    r = CcidDescriptorReport()
    if len(data) != CCID_CLASS_DESCRIPTOR_LENGTH:
        r.errors.append(
            f"descriptor is {len(data)} bytes, expected {CCID_CLASS_DESCRIPTOR_LENGTH}"
        )
        return r

    # parse.c L417-422: bLength must be 0x36.
    r.b_length = data[0]
    if r.b_length != CCID_CLASS_DESCRIPTOR_LENGTH:
        r.errors.append(f"UNSUPPORTED bLength 0x{r.b_length:02X} (parse.c:418)")
        return r

    # parse.c L424-434: bDescriptorType must be 0x21 (0xFF = proprietary).
    r.b_descriptor_type = data[1]
    if r.b_descriptor_type != CCID_CLASS_DESCRIPTOR_TYPE:
        if r.b_descriptor_type == 0xFF:
            r.notes.append("PROPRIETARY bDescriptorType (parse.c:427)")
        else:
            r.errors.append(
                f"UNSUPPORTED bDescriptorType 0x{r.b_descriptor_type:02X} (parse.c:431)"
            )
            return r

    # parse.c L436: bcdCCID printed as [3].[2].
    r.bcd_ccid = (data[3] << 8) | data[2]
    r.b_max_slot_index = data[4]

    # parse.c L438-444: voltage bits 5.0V / 3.0V / 1.8V.
    r.b_voltage_support = data[5]
    for bit, name in ((0x01, "5.0V"), (0x02, "3.0V"), (0x04, "1.8V")):
        if r.b_voltage_support & bit:
            r.notes.append(name)

    # parse.c L446-451: dwProtocols bit0 T=0, bit1 T=1.
    r.dw_protocols = _dw2i(data, 6)
    if r.dw_protocols & 0x01:
        r.notes.append("T=0")
    if r.dw_protocols & 0x02:
        r.notes.append("T=1")

    # parse.c L453-457.
    r.dw_default_clock = _dw2i(data, 10)
    r.dw_maximum_clock = _dw2i(data, 14)
    r.b_num_clock_supported = data[18]
    if r.b_num_clock_supported == 0:
        r.notes.append("clock: will use whatever is returned (parse.c:457)")
    # PORT NOTE: the GET_CLOCK_FREQUENCIES control-transfer probe
    # (parse.c L458-509) needs a live device — not applicable to a
    # static fixture.

    # parse.c L510-514.
    r.dw_data_rate = _dw2i(data, 19)
    r.dw_max_data_rate = _dw2i(data, 23)
    r.b_num_data_rates_supported = data[27]
    if r.b_num_data_rates_supported == 0:
        r.notes.append("data rate: will use whatever is returned (parse.c:514)")
    # PORT NOTE: GET_DATA_RATES probe (parse.c L515-562) — same as above.

    # parse.c L563-582.
    r.dw_max_ifsd = _dw2i(data, 28)
    r.dw_synch_protocols = _dw2i(data, 32)
    for bit, name in (
        (0x01, "2-wire protocol"),
        (0x02, "3-wire protocol"),
        (0x04, "I2C protocol"),
    ):
        if r.dw_synch_protocols & bit:
            r.notes.append(name)
    r.dw_mechanical = _dw2i(data, 36)
    for bit, name in (
        (0x01, "Card accept mechanism"),
        (0x02, "Card ejection mechanism"),
        (0x04, "Card capture mechanism"),
        (0x08, "Card lock/unlock mechanism"),
    ):
        if r.dw_mechanical & bit:
            r.notes.append(name)

    # parse.c L584-628: dwFeatures.
    r.dw_features = _dw2i(data, 40)
    for bit, name in FEATURE_BITS.items():
        if r.dw_features & bit:
            r.notes.append(f"0x{bit:08X} {name}")
    level = (r.dw_features >> 16) & 0x07
    r.notes.append(f"exchange level 0x{level:02X}: {EXCHANGE_LEVELS.get(level, 'INVALID (parse.c:609 switch has no arm)')}")
    # PORT NOTE: parse.c silently prints nothing for an unknown exchange
    # level (the switch simply falls through). A descriptor with an
    # undefined level is exactly the class this fixture gate exists to
    # catch, so the port upgrades the fall-through to an error.
    if level not in EXCHANGE_LEVELS:
        r.errors.append(
            f"exchange level bits 16..18 = 0x{level:02X} not in "
            f"{sorted(EXCHANGE_LEVELS)} (CCID Rev 1.1 Table 5.1-1 via parse.c:609-626)"
        )

    # parse.c L630-647.
    r.dw_max_ccid_message_length = _dw2i(data, 44)
    r.b_class_get_response = data[48]
    r.b_class_envelope = data[49]
    r.w_lcd_layout = (data[51] << 8) | data[50]
    r.b_pin_support = data[52]
    if r.b_pin_support & 0x01:
        r.notes.append("PIN Verification supported")
    if r.b_pin_support & 0x02:
        r.notes.append("PIN Modification supported")
    r.b_max_ccid_busy_slots = data[53]

    return r


def load_fixture(name: str) -> bytes:
    """Read a committed fixture (lowercase hex, one line)."""
    path = FIXTURE_DIR / f"{name}.hex"
    return bytes.fromhex(path.read_text().strip())
