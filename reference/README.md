# Reference library — provenance index

Every tracked external source this repo quotes from, with its upstream
pin, license, and what we use it for. This is the index required by the
Reference-First Doctrine and issue #134: quote-only copies and submodules
must carry provenance so a quote can always be traced to its authority.

Vendoring policy (what may live where) is defined in `AGENTS.md` →
"Reference-First Doctrine" → "Vendoring policy (three classes)".

## Git submodules (full upstream checkouts)

| Path | Upstream | Pin (gitlink) | Upstream license | What we quote/use |
|---|---|---|---|---|
| `reference/CCID/` | [salsa.debian.org/rousseau/CCID](https://salsa.debian.org/rousseau/CCID.git) — the pcscd CCID driver (libccid) | `1a0f75d` (1.7.1-8) | LGPL-2.1-or-later (GPL-2.0 dual) | Host-side contract: `readers/*.txt` (authoritative device identities — Cherry/CT30/K30), `src/commands.c` (bError handling; specquotes `LIBCCID`), `src/ccid_serial.c` (GemPC serial framing; specquotes `CCID_SERIAL`), `src/openct/proto-t1.c` (host T=1 engine), `src/ifdhandler.c` + `src/parse.c` (PC/SC↔CCID translation, descriptor parser), `MacOSX/reader.h` (PIN structures; specquotes `PCSC`) |
| `reference/osmo-ccid-firmware/` | [gitea.osmocom.org sim-card/osmo-ccid-firmware](https://gitea.osmocom.org/sim-card/osmo-ccid-firmware) | `639db14` (0.3-44) | GPL-2.0-or-later | Open-source CCID device reference: `ccid_common/ccid_proto.h` (CCID constants; specquotes `CCID_SPEC`), `ccid_common/iso7816_3.{c,h}` (`@implements ISO 7816-3:2006` Fi/Di/WT tables; specquotes `ISO7816`), `sysmoOCTSIM/config/hpl_sercom_config.h` (SAMD51 ISO 7816 USART config incl. NACK policy), `ccid_common/ccid_slot_fsm.c` (ICC_MUTE timeout shapes) |

## Quote-only copies (trimmed subsets, never compiled)

| Path | Upstream | Pin | Upstream license | What we quote/use |
|---|---|---|---|---|
| `reference/pcsc-lite/` | [github.com/LudovicRousseau/PCSC](https://github.com/LudovicRousseau/PCSC) — pcscd + SCard service layer | `6cadd81` (2.5.2) | BSD-3-Clause | PC/SC host-contract audit (#141): `src/winscard.c` (GetAttrib refusal + no-transmit-timeout), `src/winscard_svc.c` (daemon RPC dispatch), `src/readerfactory.c` (init capability queries), `src/eventhandler.c` (poll cadence + power-off grace), `src/prothandler.c` (PPS policy), `src/ifdwrapper.c` (IFDH call sites), `src/pcscd.h.in` (poll/grace constants), `src/PCSC/reader.h` (v2 part 10 FEATURE_* table), `src/PCSC/ifdhandler.h` (TAG_*/IFD_* codes), `src/PCSC/pcsclite.h.in` (SCARD_ATTR_* table), `doc/reader.conf.5.in`. Details: `reference/pcsc-lite/PROVENANCE.md` |
| `reference/ST/stm32f4xx-hal-driver/` | [github.com/STMicroelectronics/stm32f4xx-hal-driver](https://github.com/STMicroelectronics/stm32f4xx-hal-driver) — ST's own F4 HAL smartcard driver | `1f6451c` (master, 2026-06-05) | Apache-2.0 | Smartcard-mode framing contract (#135): `Inc/stm32f4xx_hal_smartcard.h` full (specquotes `ST_HAL` — STOP bits 0.5/1.5-only assert, NACK macros, GuardTime/baud-clock semantics, BRR math) + `Src/stm32f4xx_hal_smartcard.c` trimmed (specquotes `ST_HAL_C` — HAL_SMARTCARD_Init GTPR sequence, SMARTCARD_SetConfig, recommended ATR config). RM0090 §26.3.9 verbatim transcription still owner-gated (st.com unreachable; see `reference/ST/PROVENANCE.md`). Details: `reference/ST/PROVENANCE.md` |
| `reference/ST/stm32-mw-usb-device/` | [github.com/STMicroelectronics/stm32-mw-usb-device](https://github.com/STMicroelectronics/stm32-mw-usb-device) — ST USB Device Library CCID class | `2df324b` (master, 2026-04-08) | SLA0044-class (file headers kept) | Device-side CCID implementation reference (#138): `Class/CCID/Inc/` all five headers + `Src/usbd_ccid.c` trimmed + `Src/usbd_ccid_smartcard_template.c` (specquotes `ST_CCID` — descriptor defaults, bulk-OUT/ZLP state machine). Consumed by `docs/CCID_DESCRIPTOR_AUDIT.md`. Details: `reference/ST/PROVENANCE.md` |

## Build-path vendored code (compiled — only when patched)

| Path | Upstream | Pin | License | Why vendored |
|---|---|---|---|---|
| `vendor/synopsys-usb-otg/` | [Amperstrand/synopsys-usb-otg](https://github.com/Amperstrand/synopsys-usb-otg) | `764bc04` (master) | MIT/Apache (upstream driver) | Carries local patches that cannot live in a crates.io release; wired via `[patch.crates-io]` in the root `Cargo.toml`. The ONE legal build-path vendoring (see AGENTS.md policy). |

## Canonical git deps (NOT vendored — consumed from forks)

Unpatched dependencies are consumed as git deps from canonical Amperstrand
forks, never copied into this tree (issues #19, #6 removed the old
`vendor/mfrc522` and `vendor/iso14443-rs` divergent copies):

| Crate | Source | Pin | Shared with |
|---|---|---|---|
| `mfrc522` | `Amperstrand/mfrc522-rs` (`ai-experiments`) | rev `e9ced1e` | bolty-rs |
| `iso14443` | `Amperstrand/iso14443-rs` (`ai-experiments`, rebased on Foundation main 216d1b9) | rev `73db6d4` | bolty-rs |
| `mfrc522-pcd` | `Amperstrand/bolty-rs` | rev `3808ba9` | bolty-rs |
| `pn532-transport` | `Amperstrand/bolty-rs` | rev `5f1215da` | bolty-rs |

Pins live in `firmware/esp32-ccid/Cargo.toml`.

## Quote-lock wiring

`specquotes.toml` (checked by `.github/workflows/spec-quote-drift.yml`)
maps in-tree `// MARKER:` comments to verbatim source lines:

| Source key | File | Marker | Coverage gate |
|---|---|---|---|
| `CCID_SPEC` | `reference/osmo-ccid-firmware/ccid_common/ccid_proto.h` | `CCID_SPEC` | strict |
| `CCID_SERIAL` | `reference/CCID/src/ccid_serial.c` | `CCID_SERIAL` | strict |
| `GEMPC` | `reference/CCID/readers/GemPCTwinSerial.txt` | `GEMPC` | strict |
| `PCSC` | `reference/CCID/MacOSX/reader.h` | `PCSC` | drift-check only |
| `LIBCCID` | `reference/CCID/src/commands.c` | `LIBCCID` | drift-check only |
| `ISO7816` | `reference/osmo-ccid-firmware/ccid_common/iso7816_3.c` | `ISO7816` | drift-check only |
| `UM11495` | `docs/quote-sources/um11495-quotes.md` (verbatim transcription) | `UM11495` | drift-check only |

PDF-only authorities with no canonical text source (USB CCID Rev 1.1,
RM0090) are quoted via the closest tracked text sources above; extracted
verbatim quotes for them belong under `docs/quote-sources/` with
section/page cites (issue #140 lane). First such transcription:
`docs/quote-sources/um11495-quotes.md` — **UM11495 Rev 1.8 — 2 June
2025** (NXP PN7160 user manual,
`nxp.com/docs/en/user-guide/UM11495.pdf`, free, `user-guide` URL
segment; owner-supplied PDF is the working copy). Sections quoted:
§7.3.4 Table 74 (PH_NCI_OID_SYSTEM_WTX), §7.2 Table 22 (extended TLV),
§11.2.1 Table 99 (READER_FWITOX_NTF_CFG). The PDF itself is NOT
committed.

## Rules

1. Adding a quote source: pin provenance here FIRST (upstream repo +
   commit), then wire `[sources.X]` in `specquotes.toml`, then annotate
   the code site with the marker.
2. Submodule bumps update this table in the same commit.
3. Never compile or link anything from `reference/` — it is quote-only.
