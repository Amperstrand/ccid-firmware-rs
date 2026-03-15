# CCID Firmware Specifications Reference

This document lists the authoritative specifications and reference implementations for the CCID firmware emulator.

## Official Specifications

### USB CCID Specification

The primary specification for USB Chip/Smart Card Interface Devices.

| Property | Value |
|----------|-------|
| **Name** | Specification for Integrated Circuit(s) Cards Interface Devices |
| **Version** | Revision 1.1 |
| **Date** | April 22, 2005 |
| **Source** | [USB.org Document Library](https://www.usb.org/document-library/smart-card-ccid-version-11) |
| **Direct PDF** | [DWG_Smart-Card_CCID_Rev110.pdf](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf) |

**Key Sections:**
- Chapter 5: CCID Class Descriptor (Table 5.1-1)
- Chapter 6: CCID Message Protocol
- Chapter 7: Class-Specific Requests

### ISO/IEC 7816 Smart Card Specifications

CCID relies on ISO 7816 for physical and logical data exchange.

| Part | Title | Relevance |
|------|-------|-----------|
| Part 1 | Cards with contacts - Physical characteristics | Card dimensions and resistance |
| Part 2 | Cards with contacts - Dimensions and location of contacts | C1-C8 pinout |
| Part 3 | Cards with contacts - Electrical interface and transmission protocols | **Critical**: T=0/T=1 protocols, ATR, voltage levels |
| Part 4 | Organization, security and commands for interchange | APDU structure (CLA, INS, P1, P2, Lc, Data, Le) |

**Source:** [ISO Store](https://www.iso.org/standard/77355.html) (paid standards)

### PC/SC Specifications

Software architecture for smart card readers.

| Part | Title | Relevance |
|------|-------|-----------|
| Part 10 | IFDs with Secure PIN Entry Capabilities | **Critical**: `IOCTL_FEATURE_GET_TLV_PROPERTIES`, PIN pad features, LCD displays |

| Property | Value |
|----------|-------|
| **Version** | 2.02.09 |
| **Source** | [PC/SC Workgroup](https://pcscworkgroup.com/specifications/download/) |
| **Direct PDF** | [pcsc10_v2.02.09.pdf](http://pcscworkgroup.com/Download/Specifications/pcsc10_v2.02.09.pdf) |

### USB ICCD Specification

Lighter alternative to CCID for USB tokens with hard-wired cards.

| Property | Value |
|----------|-------|
| **Name** | Identification Cards - Integrated Circuit(s) Cards Interface Devices - Bridge with USB |
| **Version** | 1.0 |
| **Source** | [USB.org](https://www.usb.org/document-library/integrated-circuit-card-device-iccd-spec-10) |

## Reference Implementations

### CCID Free Software Driver (libccid)

The authoritative open-source CCID driver for Linux/Unix.

| Property | Value |
|----------|-------|
| **Maintainer** | Ludovic Rousseau |
| **Repository** | [salsa.debian.org/rousseau/CCID](https://salsa.debian.org/rousseau/CCID) |
| **Website** | [ccid.apdu.fr](https://ccid.apdu.fr/) |
| **License** | LGPL-2.1-or-later |

**Key Files:**
- `readers/*.txt` - Reader capability database (684+ devices)
- `src/ccid.h` - Feature flag definitions
- `src/commands.c` - CCID command handling

**Included in this project:** `reference/CCID/` (git submodule)

### osmo-ccid-firmware

CCID firmware implementation by Osmocom.

| Property | Value |
|----------|-------|
| **Maintainer** | Osmocom (sysmocom) |
| **Repository** | [gitea.osmocom.org/sim-card/osmo-ccid-firmware](https://gitea.osmocom.org/sim-card/osmo-ccid-firmware) |
| **License** | GPL-2.0-or-later |

**Key Components:**
- `ccid_common/` - Shared CCID protocol code
- `ccid_host/` - Host-side utilities
- `sysmoOCTSIM/` - Firmware for sysmoOCTSIM hardware
- `tests/` - Test patterns and protocol tests

**Included in this project:** `reference/osmo-ccid-firmware/` (git submodule)

### pcsc-lite

PC/SC middleware for Unix-like systems.

| Property | Value |
|----------|-------|
| **Maintainer** | Ludovic Rousseau |
| **Repository** | [github.com/LudovicRousseau/PCSC](https://github.com/LudovicRousseau/PCSC) |
| **Website** | [pcsclite.apdu.fr](https://pcsclite.apdu.fr/) |

**Key Headers:**
- `src/PCSC/reader.h` - PIN pad structures (`PIN_VERIFY_STRUCTURE`, etc.)
- `src/PCSC/ifdhandler.h` - IFD handler interface
- `src/PCSC/winscard.h` - WinSCard API

## Additional Resources

### EMV Contact Interface Specification

For payment card compatibility, EMV provides stricter timing requirements.

| Property | Value |
|----------|-------|
| **Source** | [EMVCo](https://www.emvco.com/emv-technologies/contact/) |
| **Document** | EMV Contact Book 1 (Application Independent ICC to Terminal Interface Requirements) |

### USB 2.0 Specification

Foundation for USB device descriptors.

| Property | Value |
|----------|-------|
| **Relevant Chapter** | Chapter 9 (USB Device Framework) |
| **Source** | [USB.org](https://www.usb.org/document-library/usb-20-specification) |

## Profile Reference Files

Device profiles in this project are aligned with the CCID reader database:

| Profile Feature | CCID Reference File |
|-----------------|---------------------|
| `profile-cherry-smartterminal-st2xxx` | `reference/CCID/readers/CherrySmartTerminalST2XXX.txt` |
| `profile-gemalto-idbridge-ct30` | `reference/CCID/readers/Gemalto_IDBridge_CT30.txt` |
| `profile-gemalto-idbridge-k30` | `reference/CCID/readers/Gemalto_IDBridge_K30.txt` |

> **Note:** The CCID project's reader files are the authoritative source of truth for device capabilities.
