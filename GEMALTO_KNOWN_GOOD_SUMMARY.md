# Gemalto + pysatochip + Seedkeeper: Known-Good Summary and Protocol

This document is the **authoritative summary** of how we achieved a full working flow: connect to a Satochip Seedkeeper in a **Gemalto** reader from the host, establish a **secure channel**, unlock with **PIN 1234**, and read the first stored secret. The first secret has **label "bacon"** and the **exported secret is a 24-word mnemonic** (e.g. BIP39/masterseed, type 0x10). It captures everything we learned, the exact protocol, and the issues we struggled with so that STM32 reader work can replicate this behavior in passthrough mode.

**Date of known-good run:** 2026-03-08  
**Host:** ubuntu@192.168.13.246  
**Reader:** Gemalto PC Twin Reader (BCF852F0)  
**Card:** Satochip Seedkeeper  
**PIN:** 1234  
**Result:** Secure channel established, PIN verified, 1 secret listed, first secret exported (id=0, type=0x10 masterseed, label='bacon', **24-word mnemonic**).

---

## 1. What We Achieved

- **ATR** received from the card (20 bytes, starts with `3B`, T=1).
- **SELECT** SeedKeeper AID: success (90 00).
- **GET_STATUS**: `needs_secure_channel=True`.
- **Initiate secure channel** (INS 0x81): ECDH key exchange with the card; host and card share session keys for encryption and MAC.
- **VERIFY_PIN** (INS 0x42) with PIN 1234: success (90 00).
- **SeedKeeper GET_STATUS**: nb_secrets=1, total_memory=8191, free_memory=8049.
- **LIST_SECRETS** (INS 0xA6): 1 secret (id=0, type=0x10, label='bacon').
- **EXPORT_SECRET** (INS 0xA2) for first secret: full secret data received. The **label is "bacon"**; the **exported secret is a 24-word mnemonic** (e.g. BIP39/masterseed). Logs may show encoded length (e.g. 244 chars hex); the human-readable form is 24 words.
- **Card disconnect** clean.

This is the **known-good baseline**: the same sequence must work over the STM32 CCID reader once it provides a correct passthrough (ATR, T=0/T=1, no protocol or secure-channel logic in firmware).

---

## 2. End-to-End Architecture

The reader is **passthrough only**. It does not implement Seedkeeper protocol or secure channel.

```
┌─────────────────────────────────────────────────────────────────────────┐
│  Host (e.g. Ubuntu)                                                      │
│  ┌─────────────────┐    ┌──────────────┐    ┌─────────────────────────┐ │
│  │ pysatochip      │───▶│ pcscd        │───▶│ libccid                 │ │
│  │ CardConnector   │    │ (PC/SC daemon)│    │ (CCID driver)           │ │
│  │ SecureChannel   │    └──────────────┘    └────────────┬────────────┘ │
│  │ (ECDH, encrypt) │                                      │              │
│  └─────────────────┘                                      │ USB          │
└────────────────────────────────────────────────────────────┼─────────────┘
                                                             ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  Gemalto reader (USB CCID)                                              │
│  Power, clock, reset, I/O; forwards APDUs to the card.                  │
└─────────────────────────────────────────────────────────────────────────┘
                                                             │
                                                             ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  Seedkeeper card (JavaCard applet)                                       │
│  ATR, SELECT, GET_STATUS, Initiate SC (0x81), Process SC (0x82),        │
│  VERIFY_PIN, LIST_SECRETS, EXPORT_SECRET. Secure channel state is        │
│  in the applet; SELECT resets it.                                        │
└─────────────────────────────────────────────────────────────────────────┘
```

- **pysatochip** (Python, on the host): builds APDUs, performs ECDH and encrypts/decrypts payloads for INS 0x82, sends everything via pyscard.
- **pcscd + libccid**: open the reader, send PowerOn / XfrBlock (APDU) to the reader, receive ATR and APDU responses.
- **Gemalto**: presents the card over USB CCID; forwards bytes to the card’s I/O; does not interpret Seedkeeper or secure channel.
- **Card**: runs Seedkeeper applet; only three commands are allowed without secure channel: GET_STATUS (0x3C), Initiate secure channel (0x81), Process secure channel (0x82). All others (including VERIFY_PIN) must be sent **inside** an encrypted 0x82 envelope when `needs_secure_channel` is true.

---

## 3. Full Protocol Description

### 3.1 Order of Operations (mandatory)

1. **SELECT application**  
   - APDU: `00 A4 04 00 0A 53 65 65 64 4B 65 65 70 65 72` (SELECT by AID "SeedKeeper").  
   - Response: 90 00 = success.  
   - **Critical:** Every SELECT **resets the secure channel** on the card (`initialized_secure_channel=false` in SeedKeeper.java `select()`). So SELECT must happen **once** before establishing the secure channel, and **never** again until you are done with the session (or you must re-do initiate secure channel after any SELECT).

2. **GET_STATUS**  
   - APDU: CLA=0xB0, INS=0x3C, P1=P2=0.  
   - Response: status bytes; byte 11 = `needs_secure_channel` (0x01 = true for Seedkeeper after personalization).  
   - Used to decide whether to run step 3.

3. **Initiate secure channel**  
   - APDU: CLA=0xB0, INS=0x81, P1=P2=0, Lc=65, data = host’s ECDH public key (65 bytes, uncompressed).  
   - Card responds with its ephemeral public key; host and card derive the same shared secret (ECDH, secp256k1 in pysatochip).  
   - Session keys (AES, MAC) are derived from that secret; used for all subsequent commands until next SELECT or power cycle.

4. **VERIFY_PIN**  
   - APDU: CLA=0xB0, INS=0x42, P1=P2=0, Lc=len(PIN), data=PIN bytes.  
   - When `needs_secure_channel` is true, this APDU is **encrypted** and sent inside a **Process secure channel** (INS 0x82) block. The reader only sees the 0x82 envelope; the card decrypts and runs VERIFY_PIN.  
   - Success: 90 00. Wrong PIN: 63 Cx (x = tries remaining). Blocked: 9C 0C.

5. **LIST_SECRETS**  
   - APDU: CLA=0xB0, INS=0xA6, P1=0, P2=0x01 (first block) then P2=0x02 (next).  
   - Response: secret headers (id, type, label, etc.) until status 9C 12 (no more).  
   - Also sent encrypted (0x82) when secure channel is active.

6. **EXPORT_SECRET**  
   - APDU: CLA=0xB0, INS=0xA2, P1=1, P2=1 (init) or P2=2 (continuation), secret_id (2 bytes).  
   - Multi-block response until full secret (e.g. masterseed, **24-word mnemonic**) is received.  
   - Encrypted via 0x82.

All application APDUs use **CLA=0xB0** (CardEdge/Seedkeeper). INS and status words are documented in [PROTOCOL_SEEDKEEPER.md](PROTOCOL_SEEDKEEPER.md) and in Satochip-Utils `constants.py` (INS_DIC, RES_DIC). Secret types and header format are in [Seedkeeper-Applet/Specifications.md](../Seedkeeper-Applet/Specifications.md).

### 3.2 Secure Channel (0x81 and 0x82)

- **0x81 (Initiate):** One-shot; host sends public key, card responds with its key; no encryption yet.
- **0x82 (Process):** Every subsequent command that requires the secure channel is sent as: plain header (CLA 0xB0, INS 0x82, …) + IV + length + **encrypted** inner APDU (e.g. VERIFY_PIN) + MAC. The card checks the MAC, decrypts, runs the inner command, encrypts the response, and sends it back in the same 0x82 response format.
- If the **card’s** secure channel state is cleared (e.g. by a second SELECT) but the **host** still uses the old keys, the card’s MAC check fails and it returns **0x9C23 (SW_SECURE_CHANNEL_WRONG_MAC)**. So keeping SELECT and secure-channel lifecycle in sync is critical.

---

## 4. What We Struggled With and How We Fixed It

### 4.1 Invalid protocol in transmit

- **Symptom:** pyscard `transmit()` raised “Invalid protocol in transmit: must be CardConnection.T0_protocol, CardConnection.T1_protocol, or CardConnection.RAW_protocol”.
- **Cause:** CardConnector’s connection came from `CardRequest.waitforcard()` (or from the CardMonitor observer) with a default protocol that was not T0 or T1 on this host.
- **Fix:** Create a **dedicated** connection to the Gemalto reader with `connect(SCARD_PROTOCOL_T0 | SCARD_PROTOCOL_T1)`, then **replace** `cc.cardservice.connection` with this connection and **release** the original connection so only one handle is active. Re-injecting this connection before each step avoids the observer overwriting `cardservice` with a Card that has no `.connection`.

### 4.2 Card is unpowered (0x80100067)

- **Symptom:** When opening a second connection to the same reader (e.g. after disconnecting the first), the host reported “Card is unpowered”.
- **Cause:** Disconnecting the first connection may have powered down the card; opening a second connection then saw no card. Alternatively, the card was not in the reader at the time of the test.
- **Fix:** Use a **single** connection: create our T0|T1 connection, **release** the one from waitforcard (disconnect + release), then inject ours. Do not open two connections to the same reader.

### 4.3 VERIFY_PIN returned 0x9C23 (wrong MAC)

- **Symptom:** After SELECT, GET_STATUS, and Initiate secure channel, VERIFY_PIN failed with status 0x9C23.
- **Cause (root cause):** In the Seedkeeper applet ([SeedKeeper.java](../Seedkeeper-Applet/src/main/java/org/seedkeeper/applet/SeedKeeper.java) line 696), `select()` sets `initialized_secure_channel=false`. So **every SELECT resets the secure channel** on the card. Our script (or pysatochip’s CardMonitor observer) was calling **card_select()** again **after** we had already called `card_initiate_secure_channel()`. That second SELECT cleared the card’s keys while the host still used the old keys; the next encrypted command (VERIFY_PIN) had a MAC computed with the old key, so the card reported **SW_SECURE_CHANNEL_WRONG_MAC (0x9C23)**.
- **Fix:**  
  1. **Kill the CardMonitor observer** as soon as CardConnector is created: `cc.cardmonitor.deleteObserver(cc.cardobserver)`, then sleep 0.5s so any in-flight callback finishes.  
  2. Run SELECT, GET_STATUS, and initiate_secure_channel **once**.  
  3. **Never** call `card_select()` again in that session before VERIFY_PIN, list, or export.  
  4. If the observer had already run and established the secure channel (`cc.sc is not None` and `cc.needs_secure_channel`), skip SELECT/GET_STATUS/init and go straight to VERIFY_PIN without calling SELECT again.

### 4.4 Observer overwrites cardservice with a Card

- **Symptom:** After we replaced the connection, a later step failed with `'Card' object has no attribute 'connection'`.
- **Cause:** CardMonitor runs in a background thread. When it detects a card event, it sets `cc.cardservice = card` (the Card object from the event). That Card does not have a `.connection` until the observer sets `card.connection = card.createConnection()`. If we had already replaced `cardservice.connection` with our gemalto_conn, the observer could still replace `cardservice` itself with a Card, and that Card had no `.connection` until the observer ran further (which could fail or use a different connection).
- **Fix:** After killing the observer, we only need one connection; we create the T0|T1 connection and inject it. We no longer rely on the observer to maintain state; we do not re-SELECT, so the observer cannot “help” by running again mid-session.

### 4.5 Distinguishing 0x9C23 from wrong PIN or blocked

- **0x9C23** = secure channel MAC error (host/card key mismatch or SC reset).  
- **0x63Cx** = wrong PIN (x = tries remaining).  
- **0x9C0C** = identity/PIN blocked.  
We confirmed in the applet source that 0x9C23 is `SW_SECURE_CHANNEL_WRONG_MAC`, so the fix was lifecycle (no SELECT after SC), not PIN or blocking.

---

## 5. Script and Run Details

- **Script:** [test_gemalto_pysatochip.py](test_gemalto_pysatochip.py)  
- **Usage:** `python3 test_gemalto_pysatochip.py 1234 --log-file /tmp/gemalto_pysatochip_log.txt`  
- **Preconditions:** pcscd running; Seedkeeper **in the Gemalto reader**; pysatochip and pyscard installed.  
- **Steps in the script:**  
  1. Patch CardRequest to force the Gemalto reader.  
  2. Create CardConnector (card_filter=\["seedkeeper"]).  
  3. **Delete the CardMonitor observer** and sleep 0.5s.  
  4. Create a T0|T1 connection to Gemalto, release the original connection, inject the new one.  
  5. Log ATR (from injected connection).  
  6. If observer did not already do it: card_select(), card_get_status(), card_initiate_secure_channel().  
  7. card_verify_PIN_simple(pin).  
  8. seedkeeper_get_status(), seedkeeper_list_secret_headers(), seedkeeper_export_secret(first id).  
  9. card_disconnect().  
- **Step log** (no secrets) can be written to a file with `--log-file` for documentation.

---

## 6. Lessons for STM32 Reader Work

- The **reader is passthrough**: it must only forward ATR and APDUs (T=0 or T=1). No secure channel, no Seedkeeper logic in firmware.
- The **host** (pysatochip) performs: SELECT, GET_STATUS, initiate secure channel (0x81), then all subsequent commands (VERIFY_PIN, LIST_SECRETS, EXPORT_SECRET) inside encrypted 0x82.
- **Protocol:** T=1 for this card (ATR indicates T=1); full T=1 block handling (I-blocks, R-blocks, S-blocks, LRC, chaining, IFSD) must work so that 0x82 payloads are delivered correctly.
- **Do not** call SELECT after the host has established the secure channel; the STM32 does not call SELECT—the host does. The firmware only forwards what the host sends.
- **Known-good reference:** Gemalto + pysatochip + Seedkeeper (PIN 1234) → first secret (label “bacon”, 24-word mnemonic) listed and exported. Use this to validate any STM32 reader: same host, same pysatochip script, reader name switched to the STM32 (e.g. “OMNIKEY AG CardMan 3121” when the STM32 presents as 076B:3021); expect the same sequence to succeed once the STM32 presents a correct ATR and T=1 APDU path. **This baseline is now verified on the STM32 reader** (see Section 8).

---

## 7. References

- [PROTOCOL_SEEDKEEPER.md](PROTOCOL_SEEDKEEPER.md) — INS/RES, status words, sequence.  
- [LEARNINGS.md](LEARNINGS.md) — M0/M1/M2/M3, OMNIKEY control test, Gemalto + pysatochip result.  
- [Seedkeeper-Applet/Specifications.md](../Seedkeeper-Applet/Specifications.md) — Secret types, header format, export policy.  
- [Seedkeeper-Applet/.../SeedKeeper.java](../Seedkeeper-Applet/src/main/java/org/seedkeeper/applet/SeedKeeper.java) — `select()` resets `initialized_secure_channel`; 0x9C23 = SW_SECURE_CHANNEL_WRONG_MAC.  
- [Satochip-Utils/constants.py](../Satochip-Utils/constants.py) — INS_DIC, RES_DIC.  
- [Satochip-Utils](../Satochip-Utils/) — GUI that uses pysatochip (CardConnector, secure channel, list/export).  
- pysatochip (CardConnector, SecureChannel, card_initiate_secure_channel, card_encrypt_secure_channel) — host-side implementation.

---

## 8. STM32 reader verification

The same known-good flow has been **verified on the STM32 CCID reader** (2026-03-08).

- **Host:** Same (e.g. ubuntu@192.168.13.246).
- **Reader:** OMNIKEY AG CardMan 3121 (STM32 firmware, USB VID:PID 076B:3021).
- **Flow:** ATR → SELECT → GET_STATUS → Initiate secure channel → VERIFY_PIN (1234) → SeedKeeper GET_STATUS → LIST_SECRETS → EXPORT_SECRET.
- **Result:** One secret, **label "bacon"**, **24-word mnemonic**; **KNOWN_GOOD** on STM32 as well as Gemalto.
- **Script:** Same [test_gemalto_pysatochip.py](test_gemalto_pysatochip.py) with `--reader OMNIKEY` (e.g. `python3 test_gemalto_pysatochip.py 1234 --reader OMNIKEY --log-file /tmp/stm32_test_log.txt`).

See [LEARNINGS.md](LEARNINGS.md) “STM32 CCID reader: full success” for the two critical T=1 firmware fixes (sequence number persistence, I-block PCB detection) that made this possible.
