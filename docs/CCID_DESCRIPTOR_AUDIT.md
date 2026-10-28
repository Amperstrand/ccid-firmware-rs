# CCID descriptor / wire conformance audit (issue #138)

**Method**: field-by-field comparison of our three device profiles
(`firmware/ccid-firmware/src/device_profile.rs`, the single source of truth —
the 52-byte descriptor is generated from these consts and the committed
fixtures under `tests/tools/fixtures/ccid/` are byte-compared against them in
CI) against:

- **Authority** — `reference/CCID/readers/*.txt`: the commercial readers we
  emulate, parsed by libccid's own `parse.c` dump.
- **Implementation reference** — ST's USB Device Library CCID class
  (`reference/ST/stm32-mw-usb-device/`, quote-only, pin in
  `reference/ST/PROVENANCE.md`): what a commercial STM32 CCID reader does.
- **Host consumer** — libccid (`reference/CCID/src/`): what the host does
  with each field.

Date: 2026-10-28. Author: TRACK F lane (issue #138), no hardware in the loop
(descriptor bytes are host-testable; wire-behaviour verdicts cite the
libccid source).

## 1. Descriptor fields (52-byte class descriptor)

| Field (offset) | Ours (Cherry) | Authority Cherry | Ours (CT30/K30) | Authority CT30/K30 | ST template default | Verdict |
|---|---|---|---|---|---|---|
| bcdCCID (0) | 0x0100 | 1.00 | 0x0101 | 1.01 | 1.10 (`USBD_CCID_DESC`… rev field) | ✅ match per profile (reference-locked) |
| bMaxSlotIndex (2) | 0 | 0x00 | 0 | 0x00 | 0 (1 slot) | ✅ |
| bVoltageSupport (3) | 0x01 (5V) | 0x01 | 0x07 (from BASE) | 0x07 | 0x07 | ⚠️ **CT30/K30 verdict: matches authority; Cherry matches authority. Note: BASE_PROFILE 0x07 is overridden correctly per profile.** ✅ |
| dwProtocols (4) | 0x0003 (T=0\|T=1) | same | 0x0003 | same | 0x03 | ✅ |
| dwDefaultClock (8) | 4000 kHz | 4.000 MHz | 4800 | 4.800 MHz | 3600 (`USBD_CCID_DEFAULT_CLOCK_FREQ`) | ✅ match authority (ST default is its own eval-hw value) |
| dwMaximumClock (12) | 8000 | 8.000 MHz | 4800 | 4.800 MHz | = default | ✅ |
| bNumClockSupported (16) | 0 | 0 (req times out) | 0 | 0 (req: Success, table empty→…) | 0 | ✅ our GET_CLOCK_FREQUENCIES answers 1 entry; authority Cherry TIMES OUT — cosmetic delta, host tolerates both (ccid_usb.c falls back to dwDefaultClock) |
| dwDataRate (17) | 10753 | 10753 | 12903 | 12903 | 9677 | ✅ |
| dwMaxDataRate (21) | 344105 | 344105 | 825806 | 825806 | = default | ✅ — and load-bearing for #122: this ceiling is what gates libccid's PPS decision (ifdhandler.c:885-887) |
| bNumDataRatesSupported (25) | 0 | 0 (req times out) | 0 | **53** (GET_DATA_RATES answers 53 rates) | 0 | ⚠️ **finding F2** (below) for CT30/K30; Cherry ✅ |
| dwMaxIFSD (26) | 254 | 254 | 254 | 254 | 254 | ✅ |
| dwSynchProtocols (30) / dwMechanical (34) | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | ✅ |
| dwFeatures (38) | 0x000100BA | 0x000100BA | 0x00010230 | 0x00010230 | TPDU level (`EXCHANGE_LEVEL_FEATURE = TPDU_EXCHANGE`) | ✅ byte-locked by `test_cherry_st2100_dwfeatures_reference_exact_0x000100ba` + `test_all_profiles_identity_and_capabilities` |
| dwMaxCCIDMessageLength (42) | 270 | 270 | 271 (from BASE) | 271 | 271 (`CCID_MAX_BLOCK_SIZE_HEADER`) | ⚠️ **finding F1** (below) for Cherry |
| bClassGetResponse/bClassEnvelope (46/47) | 0xFF/0xFF | FF/FF | 0x00/0x00 | 0x00/0x00 | 0xFF-class | ✅ |
| wLcdLayout (48) | 0x0000 | 0x0000 | 0x0000 | 0x0000 | — | ✅ |
| bPINSupport (50) | 0x03 | 0x03 | 0x00 | 0x00 | — | ✅ (the K30 false-PINpad incident is test-locked shut) |
| bMaxCCIDBusySlots (51) | 1 | 1 | 1 | 1 | 1 | ✅ |

## 2. Findings

- **F1 — Cherry dwMaxCCIDMessageLength 270 vs BASE 271.** The Cherry
  profile pins 270 (authority: readers/*.txt "270 bytes"); BASE_PROFILE and
  the Gemalto profiles carry 271 (their authority). Both are legal
  (spec: ≤ 271 short-APDU/TPDU). Our firmware rejects XfrBlock with
  declared dwLength > 261 (`ccid_core.rs` oversize guard) — under the
  Cherry identity the host should never send a CCID message > 270 total
  (= 260-byte payload). libccid sizes its buffers from the descriptor, so
  the two agree. No action; recorded because the two profiles deliberately
  differ.
- **F2 — CT30/K30 advertise bNumDataRatesSupported=0 while the real
  devices answer GET_DATA_RATES with a 53-entry table.** Consequence in
  libccid: `ccid_usb.c:810-814` — table NULL when the count is 0 → PPS
  rate choice gated ONLY by `dwMaxDataRate` (ifdhandler.c:885-899). The
  real device's table makes the host pick a table-supported rate; ours
  accepts any ≤ 825806. For the wire this is UNSUPPORTED-BEHAVIOUR drift,
  not a correctness break, EXCEPT it feeds **F3**.
- **F3 — host-owned PPS on the Gemalto profiles (dwFeatures 0x230: no bit
  7) vs our reader-side wire-rate guard (#122).** libccid RUNS
  `PPS_Exchange` through our TPDU pipe (ifdhandler.c:982 passes when
  AUTO_PPS_CUR is clear), the card switches to the PPS1 rate, and the host
  then sends SetParameters carrying the new Fi/Di — which our new
  `set_wire_timing` deliberately refuses to apply unless the rate is
  proven on our side (the f5feb61 class guard). On the Cherry (bit 7 set)
  that guard is exactly right; on CT30/K30 the host-PPS path means the
  card CAN be at a rate we never negotiated → the reader wire desyncs
  until reconnect. **Bench-gated follow-up**: make the guard
  profile-aware (bit 7 = reader-owned PPS; bit 7 clear = trust host
  SetParameters unconditionally, like the real CT30 does). Tracked with
  issue #122 (re-land lane); no hardware in this session to prove it.
- **F4 — GET_CLOCK_FREQUENCIES/GET_DATA_RATES control answers.** The real
  Cherry times out on both; we answer with one entry each. libccid treats
  a timeout as "no table" (NULL) and our count-0 descriptor ALSO yields
  NULL in the host — convergent behaviour, no action. (ST's template
  answers from its own tables; irrelevant to our identities.)

## 3. Bulk endpoint sizing + ZLP

- **Endpoint size**: we allocate bulk IN/OUT at `PACKET_SIZE` (64, FS) —
  same as ST's `CCID_BULK_EPIN_SIZE 64U` and interrupt-IN at 8-10 bytes
  (ST: `CCID_INT_BUFF_SIZ 2U` payload). ✅
- **ZLP emission**: our `ccid.rs` `try_send` emits a ZLP whenever the
  response is an exact multiple of 64 (`needs_zlp`). ST's template OUT
  state machine treats a received ZLP as an idle transition
  (`usbd_ccid.c`: `CurrPcktLen == 0x00U /* Zero Length Packet Received
  */ → CCID_STATE_IDLE`) and completes exact-multiple transfers by
  length — symmetric, spec-conformant on both sides. On the host side,
  libccid's ZLP machinery (`ccid.c:62-73`, `ENABLE_ZLP`) only *fixes up*
  GEMPCTWIN/GEMPCKEY/DELLSCRK bcdDevice-2.00 chipsets — none of our three
  identities — so no host expects us to OMIT the ZLP. Verdict: our ZLP
  behaviour is correct and identity-safe. ✅
- **OUT reassembly**: ours is dwLength-driven (`message_ready` +
  un-assemblable resync drop, `#268`); ST's is short-packet-or-exact-
  length driven (`CCID_STATE_RECEIVE_DATA`). Both conform; ours
  additionally survives the HIL Leg-3 wedge class.

## 4. T=0/T=1 engine placement (ST template study)

ST's `usbd_ccid_smartcard_template.{c,h}` ships a full T=0/T=1 state
machine INSIDE the reader (SC_*, TAL_* layers). We deliberately differ
— reader-role doctrine (AGENTS.md): at TPDU exchange level the HOST owns
T=1 (blocks relayed byte-transparent via `transmit_raw`), only T=0
procedure-byte handling and activation (ATR/PPS/IFS at power-on) are
device-side. The ST template's own bulk-transfer framing (§3) and
descriptor layout (§1) are what we conform to; its protocol-ownership
model is what a SHORT-APDU-level reader would need, not us.
