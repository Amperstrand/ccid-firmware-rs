# UM11495 Rev 1.8 — verbatim quote source (PDF-only authority)

Provenance: NXP UM11495, "PN7160 NFC controller — User manual", Rev 1.8 —
2 June 2025, © 2025 NXP B.V.
URL: https://www.nxp.com/docs/en/user-guide/UM11495.pdf (free download;
note the `user-guide` URL segment — bot-gated for fetchers, owner-supplied
PDF is the working copy).

This file is the tracked text of the sections this repo quotes (issue
#140 lane / issue #156 item D). Extracted with `pdftotext` from the
owner's Rev 1.8 PDF on 2026-10-14; section/page numbers refer to that
revision (older public copies are Rev 1.6 where §7.3.4/Table 74 appear
as §9.3.4/Table 21 — cite Rev 1.8).

Quoted by `specquotes.toml` → `[sources.UM11495]` (marker `UM11495`)
from `crates/pn7160-nci/src/lib.rs`.

## §7.3.4 (p.69) — [PN7160-NCI] extension: WTX notification

> After data was sent to the card/tag, it can request an additional processing time before sending data response. This is done with WTX (Waiting Time Extension) request. If WTX REQ/RESP exchange phase continues an NCI system notification WTX is sent with a period configurable via READER_FWITOX_NTF_CFG.

Table 74. PH_NCI_OID_SYSTEM_WTX:

> GID: 1111b
> OID: 0x17
> 0 (parameters)
> Notification indicating that RF communication is in phase of WTX(RTOX) REQ/RESP exchange for longer period of time.

## §7.2 (p.39) — Table 22. Extended TLV for proprietary parameters

> Regarding the PN7160 needs, this reserved area is not sufficient. To extend this space, the solution chosen is to define a space of Tags coded on 16 bits, instead of 8 bits. These extended Tags will always start by the value 0xA0, which is the first value available in the Proprietary range. This allows adding 256 new parameters.

Table 22 fields:

> Tag = 0xA0XX | 2 bytes | Extended tag identifier
> Len | 1 byte | The length of Val (m)
> Val | m bytes | Value of the configuration parameter
> Total: m+3 bytes

## §11.2.1, Table 99 (p.102) — READER_FWITOX_NTF_CFG row

> READER_FWITOX_NTF_CFG — RW in E²PROM
> Tag: 0xA0 0x65 — Len: 2 — Default Value: 0x1000
> Delay before sending TOX notification to Host. Unit is 4096/fc (~302us)
> READER_FWITOX_NTF_CFG | RW in E²PROM | 0xA0 0x65 | 2 | 0x1000
