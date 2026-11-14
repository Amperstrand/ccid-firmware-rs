# reference/nxp-nci/ — quote-only excerpts (PROVENANCE)

Upstream: [github.com/NXPNFCLinux/linux_libnfc-nci](https://github.com/NXPNFCLinux/linux_libnfc-nci)
— NXP's production NCI stack headers, shipped in every Android NXP NFC
stack; the PN7160's silicon follows these tables.

Pin: commit `8476ea1a4091e6facc37e028d8489cc2ef4741ba` (2018-08-13,
"R2.4 release") — the last commit touching
`src/libnfc-nci/hal/include/nci_defs.h` (the repo has been quiet since;
verify: `gh api repos/NXPNFCLinux/linux_libnfc-nci/commits?path=src/libnfc-nci/hal/include/nci_defs.h&per_page=1`).

License: Apache-2.0 (Broadcom 1999-2014 / NXP 2013-2014 headers kept
verbatim). Quote-only — never compiled, never linked (class-1 reference
copy per the AGENTS.md vendoring policy).

## Files

| File | Original path | Kept | What we quote |
|---|---|---|---|
| `nci_defs.h` | `src/libnfc-nci/hal/include/nci_defs.h` | full | The NXP-side constants `crates/pn7160-nci` cross-checks (#142): `NCI_PARAM_ID_TOTAL_DURATION 0x00` / `NCI_PARAM_LEN_TOTAL_DURATION 2` (the founding incident's 1-octet param-ID reference), `NCI_ISO_DEP_MAX_INFO 253` (MAX_DATA_FRAGMENT), `NCI_DEACTIVATE_REASON_*` (the bench-captured 0x02 = RF_LINK_LOSS table, PR #542), `NCI_MSG_RF_WTX 0x17`, `NCI_INTERFACE_ISO_DEP` / `NCI_PROTOCOL_ISO_DEP`, LA_* listen-config param IDs (0x30/0x31/0x32) |

## Maintenance

On a re-pin, re-copy and update this file in the same commit — the
spec-quote-drift CI leg then re-verifies every `// NCI_DEFS:` quote
(source: `[sources.NCI_DEFS]` in `specquotes.toml`).
