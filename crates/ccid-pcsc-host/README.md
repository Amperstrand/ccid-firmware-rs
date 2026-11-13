# ccid-pcsc-host

Host-side PC/SC client for Amperstrand CCID readers: reader discovery,
verified APDU case forms, exchange + status-word split.

## Why

The bench tooling (`host-tools/`, the conformance batteries, golden-trace
capture) shares one hardened client instead of per-script `pyscard`
copies: APDU case forms are **verified against the wire contract**
(case-1/2/2e/3/3e lengths — the #450 frame-bounds lineage), exchanges
split R-APDU data from SW with typed errors (no stringly results), and
reader matching is by stable identity substring, not index.

## Example

```rust
use ccid_pcsc_host::{apdu, PcscCard};

let card = PcscCard::connect_by_name("Nucula")?;
let atr = card.atr()?;
// verified case-4 SELECT (lengths checked before the wire)
let cmd = apdu(0x00, 0xA4, 0x04, 0x00,
               &[0xD2, 0x76, 0x00, 0x00, 0x85, 0x01, 0x01, 0x00])?;
let (data, sw) = card.transmit(&cmd)?;
if sw == 0x9000 { /* NDEF applet selected */ }
```

## Status

Bench-proven against the nucula (PN7160) and m5stick (MFRC522) readers
over both transports (GemPC serial via broker, vpcd over Wi-Fi). MSRV
1.99. License GPL-2.0-or-later. Part of
[ccid-firmware-rs](https://github.com/Amperstrand/ccid-firmware-rs).
