# SeedKeeper protocol reference

Short reference for the SeedKeeper applet protocol so the CCID reader work can assume “reader is passthrough; host does this sequence.” Full spec: [Seedkeeper-Applet/Specifications.md](../Seedkeeper-Applet/Specifications.md). Client flow: [Satochip-Utils](../Satochip-Utils/) (controller + pysatochip CardConnector).

## APDU sequence (host-side)

Reader is **passthrough only**. The host (e.g. pysatochip) must:

1. **SELECT** SeedKeeper AID: `00 A4 04 00 Lc <AID>` with AID `53 65 65 64 4B 65 65 70 65 72` (ASCII "SeedKeeper").
2. **GET_STATUS** (card/applet): CLA=0xB0, INS=0x3C, P1=P2=0. Response includes byte 11: `needs_secure_channel` (0x01 = true). Used to decide whether to run step 3.
3. **Initiate secure channel** (if `needs_secure_channel`): CLA=0xB0, INS=0x81, host sends its ECDH public key; card responds with card public key; host derives shared secret and uses it for encryption/MAC (INS 0x82 for subsequent encrypted APDUs). See pysatochip `SecureChannel` and `card_initiate_secure_channel` / `card_encrypt_secure_channel`.
4. **VERIFY_PIN**: CLA=0xB0, INS=0x42, P1=P2=0, Lc=pin length, data=PIN bytes. **Must be done after secure channel** if applet requires it (otherwise SW=9C20).
5. **LIST_SECRETS**: CLA=0xB0, INS=0xA6, P1=0, P2=0x01 (init) then P2=0x02 (next). Response: secret headers until SW=9C12 (no more).
6. **EXPORT_SECRET**: CLA=0xB0, INS=0xA2, P1=1 P2=1 (init) or P1=1 P2=2 (update), secret_id 2 bytes. Multi-step until full secret exported.

All application APDUs use **CLA=0xB0** (CardEdge / SeedKeeper).

## INS codes (from Satochip-Utils constants)

| INS  | Name              | Note                          |
|------|-------------------|-------------------------------|
| 0x3C | GET_STATUS        | Card/applet status            |
| 0x40 | Create PIN        | Setup                         |
| 0x42 | Verify PIN        | Unlock                        |
| 0x44 | Change PIN        |                               |
| 0x46 | Unblock PIN       |                               |
| 0x81 | Initiate SC       | Secure channel (ECDH)         |
| 0x82 | Encrypted APDU    | Wrapped command/response     |
| 0xA0 | Generate masterseed |                           |
| 0xA2 | Export secret     | Init (P2=1) / Update (P2=2)   |
| 0xA5 | Reset secret      | Delete                        |
| 0xA6 | List secrets      | P2=1 init, P2=2 next          |
| 0xA7 | SeedKeeper status | Memory/logs                   |
| 0xAE | Generate 2FA      |                               |
| 0xA1 | Import secret     |                               |
| 0xFF | RESET TO FACTORY  |                               |

## Status words (RES_DIC and common)

| SW     | Meaning                  | Note                                |
|--------|--------------------------|-------------------------------------|
| 0x9000 | OK                       | Success                              |
| 0x63Cx | PIN failed               | x = attempts remaining              |
| 0x9C03 | Operation not allowed   |                                      |
| 0x9C04 | Setup not done          | Card not personalized               |
| 0x9C05 | Feature unsupported     |                                      |
| 0x9C01 | No memory left          |                                      |
| 0x9C08 | Secret not found        |                                      |
| 0x9C0C | Identity blocked        | PIN blocked                          |
| 0x9C12 | No more objects         | End of LIST_SECRETS                  |
| **0x9C20** | **Secure channel required** | Host must establish SC before VERIFY_PIN |
| 0x9C21 | Secure channel not initialized | Sent if SC expected but not done |
| **0x9C23** | **Wrong MAC (secure channel)** | SW_SECURE_CHANNEL_WRONG_MAC in SeedKeeper.java; MAC verification fails in INS 0x82 (e.g. host/card key mismatch or SC reset by SELECT). Do not call card_select() after initiating SC. |
| 0x9C30 | Lock error              |                                      |
| 0x9C31 | Export not allowed      | Policy on card                       |

## Secret types (Specifications.md)

- 0x10 Masterseed, 0x30 BIP39 mnemonic, 0x40 Electrum mnemonic, 0x70 Pubkey, 0x90 Password, 0xB0 2FA, 0xC0 Data, 0xC1 Descriptor. See [Seedkeeper-Applet/Specifications.md](../Seedkeeper-Applet/Specifications.md) for header layout and export policy.

**Reference test secret:** The secret used for verification in this project has **label "bacon"**, is a **24-word mnemonic** (masterseed), and is unlocked with **PIN 1234**.

## Where implemented

- **Applet:** [Seedkeeper-Applet](../Seedkeeper-Applet/) (JavaCard).
- **Client:** [Satochip-Utils](../Satochip-Utils/) controller + [pysatochip](https://github.com/Toporin/pysatochip) CardConnector and SecureChannel. INS/RES constants: [Satochip-Utils/constants.py](../Satochip-Utils/constants.py).
