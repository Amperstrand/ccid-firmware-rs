# reference/linux-nci/ — quote-only excerpts (PROVENANCE)

Upstream: [github.com/torvalds/linux](https://github.com/torvalds/linux)
— the kernel NCI stack, this repo's doctrinal NCI reference (AGENTS.md
"Reference-First Doctrine" reference library).

Pin: commit `9907cda95fcbf44141b1292faab89cf8ec542f22` (2024-11-07,
"net: nfc: Propagate ISO14443 type A target ATS to userspace via
netlink") — the last commit touching `include/net/nfc/nci.h` when this
copy was made (2026-11-14). Verify: `gh api
repos/torvalds/linux/commits?path=include/net/nfc/nci.h&per_page=1`.

License: GPL-2.0-only (kernel SPDX headers kept verbatim). Quote-only —
never compiled, never linked (class-1 reference copy per the AGENTS.md
vendoring policy).

## Files

| File | Original path | Kept | What we quote |
|---|---|---|---|
| `nci.h` | `include/net/nfc/nci.h` | full | Every NCI constant `crates/pn7160-nci` emits/parses (#142): status codes (NCI_STATUS_*, incl. DISCOVERY_ALREADY_STARTED 0xa0), MT bits, GIDs, opcodes (NCI_OP_*), RF technologies/protocols/interfaces, deactivation types, `struct set_config_param` (1-octet param IDs — the founding-incidence reference), `struct nci_rf_intf_activated_ntf`, NCI_CTRL_HDR_SIZE, nci_pbf/nci_conn_id |
| `core.c` | `net/nfc/nci/core.c` | trimmed (nci_rx_work MT dispatch) | The RX-side "unknown MT" drop — the quote behind pn7160-nci's `Frame::decode` RFU-MT rejection (fuzz 2026-10-26 fix) |

## Maintenance

On a re-pin (kernel NCI constants evolve rarely), re-copy at the new
commit and update this file in the same commit — the spec-quote-drift
CI leg then re-verifies every `// NCI_KERN:` quote against the new
revision (sources: `[sources.NCI_KERN]` in `specquotes.toml`, files
`nci.h` + `core.c`).
