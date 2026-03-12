# CCID reader firmware — progress and learnings

Short dated entries per milestone: what was done, what was observed (e.g. defmt), what we concluded, and what's next. See [DEFMT_DIAGNOSIS.md](DEFMT_DIAGNOSIS.md) for activation checklist and defmt capture details.

---

## M0: CLK-first activation and ATR timeout SR telemetry (current state)

**Done:**
- CLK asserted before RST: explicit `set_clock(true)` and "CLK enabled before RST" log in `power_on()` before driving RST.
- ATR wait log: "ATR: waiting for first byte (timeout N ms)" so we confirm we're in the first-byte path.
- SR telemetry on ATR timeout: when no byte is received within timeout, defmt logs `ATR timeout SR=0xXXXX (FE= PE= ORE= NE=)` (framing, parity, overrun, noise).
- Activation checklist added in [DEFMT_DIAGNOSIS.md](DEFMT_DIAGNOSIS.md): C1 (VCC), C2 (RST), C3 (CLK), C7 (I/O) and OMNIKEY control test.
- Delays: 50 ms power-on, 20 ms post-RST; NACK off during ATR, on again for T=0 only; `clear_usart_errors()` before waiting for first byte.

**Observed (defmt):**
- `ATR timeout SR=0x01C0 (FE=0 PE=0 ORE=0 NE=0)` — no framing/parity/overrun/noise errors → I/O line likely never toggled (no clock at card or card not driving I/O).

**Conclusion:**
- Failure is at activation layer (power/CLK/RST/I/O), not protocol or baud. Next: OMNIKEY control test (same Seedkeeper in reference reader) and/or scope C3 (CLK), C7 (I/O) per activation checklist.

**Next:** M1 = TS received, M2 = full ATR + PowerOn success, M3 = Seedkeeper test passes. *(All achieved 2026-03-08; see "STM32 CCID reader: full success" below.)*

**Post CLK-to-RST delay (external research):** Firmware now uses `SC_CLK_TO_RST_DELAY_MS = 15` (ISO 7816-3: min 40k clock cycles after CLK before RST high). Built, flashed to remote (2026-03-08). Reader enumerates as OMNIKEY AG CardMan 3121 (076B:3021); pcsc_scan shows Reader 1 = STM32, Reader 0 = Gemalto. Card was in Gemalto (Reader 0 had ATR); STM32 reader showed "Card removed". `test_seedkeeper.py OMNIKEY 1234` → "No smart card inserted" (0x8010000C). To verify whether the delay helps ATR: insert card in STM32 reader and re-run pcsc_scan + test_seedkeeper (or capture defmt during connect).

---

## OMNIKEY control test

**Goal:** See whether the same Seedkeeper works in the real OMNIKEY. If ATR is present there → STM32 activation/hardware is the prime suspect; if card is also unpowered there → suspect card/contact/health.

**Procedure:** Move the **same** Seedkeeper from the STM32 reader to the Gemalto/OMNIKEY 3021 (or other reference reader). Run `pcsc_scan` (or equivalent). Record: ATR present or not, any error.

**Result:** *(2026-03-08)* On host `ubuntu@192.168.13.246`, after ensuring **pcscd** was running (`sudo systemctl start pcscd`), `pcsc_scan` found:
- **Reader 0:** Gemalto PC Twin Reader (BCF852F0) 00 00
- **Card state:** Card inserted
- **ATR:** `3B FA 18 00 00 81 31 FE 45 4A 54 61 78 43 6F 72 65 56 31 B2` (full ATR, TS=3B, T=1, TCK correct)
- **Note:** This ATR corresponds to a different card (historical bytes `4A 54 61 78 43 6F 72 65 56 31` → "JTaxCoreV1"; identified as Fiji Revenue & Customs taxpayer portal), not the SatoChip Seedkeeper. So either (1) the Seedkeeper was swapped for this card, or (2) a different slot/card was read. Conclusion: **pcscd + Gemalto reader work;** a card in the Gemalto returns a full ATR. If the Seedkeeper had been in the reader and did *not* respond (no ATR, "doesn't light up"), that could be due to pcscd not running or to possible card damage (see RESEARCH_AGENDA §5.7). To confirm Seedkeeper in Gemalto: insert Seedkeeper, run `pcsc_scan` again, and record whether Seedkeeper ATR appears or not.

**Seedkeeper test in Gemalto (same run):** `python3 test_seedkeeper.py Gemalto 1234` — **Connect OK**, ATR as above; **SELECT SeedKeeper OK**; **VERIFY_PIN failed SW=9C20**. So the card has the SeedKeeper applet but PIN 1234 was rejected (or 9C20 means something else). We must get unlock + read secret working in Gemalto first; see RESEARCH_AGENDA §5.8 and EXTERNAL_RESEARCH_REQUEST “Seedkeeper applet verification”.

**Hardware verification (scope):** If scope or logic analyzer is available, check C1 (VCC), C2 (RST), C3 (CLK), C7 (I/O) per [DEFMT_DIAGNOSIS.md](DEFMT_DIAGNOSIS.md) activation checklist. If not available: document here and rely on OMNIKEY result and firmware experiments. *(Scope: not run yet.)*

SW=9C20 means the SeedKeeper applet requires a **host-side** secure channel before VERIFY_PIN; the reader is passthrough only. To unlock and read secrets, use **pysatochip** on the host (see [EXTERNAL_RESEARCH_REQUEST.md](EXTERNAL_RESEARCH_REQUEST.md) §5).

### Gemalto + pysatochip verification (secure channel, unlock, read secret)

**Goal:** Confirm we can establish a secure channel to the SeedKeeper in the **Gemalto** reader, unlock with PIN 1234, and read a secret using **pysatochip** (before debugging STM32/pin mapping).

**Procedure:**
1. On the host with Gemalto (e.g. `ubuntu@192.168.13.246`): ensure **pcscd** is running (`sudo systemctl start pcscd`).
2. Insert **SeedKeeper** in the **Gemalto** reader only (remove from STM32 if present).
3. Run: `python3 test_gemalto_pysatochip.py` (or `python3 test_gemalto_pysatochip.py 1234` for PIN 1234).  
   The script forces the Gemalto reader via a CardRequest patch, then uses pysatochip’s CardConnector (card_filter=\["seedkeeper"\]) to: SELECT SeedKeeper → GET_STATUS → initiate secure channel → VERIFY_PIN → list secrets → export first secret.
4. **Expected:** All steps complete and “Secure channel + unlock + read secret OK on Gemalto with pysatochip.”  
   **If VERIFY_PIN fails (e.g. wrong PIN or 9C20):** Secure channel is required; the script establishes it; if it still fails, note the exact SW and document in LEARNINGS.

**Result:** *(2026-03-08)*  
- Script `test_gemalto_pysatochip.py` added: forces Gemalto reader, creates T0|T1 connection to Gemalto first then injects it into CardConnector, runs SELECT → GET_STATUS → initiate secure channel → VERIFY_PIN → list/export secret. Step log via `--log-file path`.  
- **Fix (same day):** Use a single active connection: create our T0|T1 connection, then release the connection from `waitforcard()` (disconnect + release) and inject ours; re-inject before each step so CardMonitor observer cannot break use (it can replace `cc.cardservice` with a Card that has no `.connection`).
- **Root cause of 0x9C23:** In SeedKeeper applet ([SeedKeeper.java](Seedkeeper-Applet/src/main/java/org/seedkeeper/applet/SeedKeeper.java) line 696), `select()` sets `initialized_secure_channel=false`. So every SELECT resets the secure channel on the card. The script was (1) replacing the connection and (2) calling card_select() after the observer had already run; the observer could also call card_select() after we initiated SC, so host and card keys went out of sync → **SW 0x9C23 = SW_SECURE_CHANNEL_WRONG_MAC** (wrong MAC in INS 0x82), not wrong PIN. Wrong PIN is 0x63Cx; blocked is 0x9C0C.
- **Script fix (0x9C23):** Remove observer immediately after CardConnector init (`cc.cardmonitor.deleteObserver(cc.cardobserver)`), sleep 0.5s. On this host the default connection had invalid protocol for transmit, so the script still creates a T0|T1 connection and injects it (then releases the original). Only run SELECT + GET_STATUS + initiate_secure_channel once; never call card_select() after initiating the secure channel.
- **Remote run with Seedkeeper in Gemalto** (ubuntu@192.168.13.246), before fix:
  - ATR: `3BFA1800008131FE454A546178436F72655631B2` (20 bytes) ✓  
  - SELECT SeedKeeper ✓  
  - GET_STATUS ✓ (`needs_secure_channel=True`)  
  - Initiate secure channel ✓  
  - VERIFY_PIN failed with **SW=0x9C23** (secure channel wrong MAC; root cause above).  
- **Known-good run (2026-03-08, after script fix):** Same host, Seedkeeper in Gemalto. `python3 test_gemalto_pysatochip.py 1234 --log-file /tmp/gemalto_pysatochip_log.txt` → **full success**: ATR ✓, SELECT ✓, GET_STATUS ✓, Initiate secure channel ✓, VERIFY_PIN ✓, SeedKeeper GET_STATUS (nb_secrets=1, total_memory=8191, free_memory=8049), LIST_SECRETS (1 secret), EXPORT first secret (id=0, type=0x10, label='bacon'), SECRET_HEX_LEN 244 chars, card_disconnect. **KNOWN_GOOD: Secure channel + unlock + read secret OK on Gemalto with pysatochip.**
- **Known-good status: YES.** Gemalto + pysatochip + Seedkeeper (PIN 1234) verified; use this as reference for STM32 reader work.
- **Full summary:** See [GEMALTO_KNOWN_GOOD_SUMMARY.md](GEMALTO_KNOWN_GOOD_SUMMARY.md) for a detailed description of the protocol, what we struggled with (0x9C23, observer race, connection protocol), how we fixed it, and lessons for STM32 reader work.
- Protocol and INS/RES are documented in [PROTOCOL_SEEDKEEPER.md](PROTOCOL_SEEDKEEPER.md).

**Note (REPL / USART):** Research found that REPL uses **USART3** on the STM32 board. The smartcard interface uses **USART2** (e.g. PA2/PA4). So there is a pin mapping consideration when debugging the STM32 reader (REPL on USART3 vs smartcard on USART2); confirm Gemalto + pysatochip first, then return to STM32 activation/pin mapping.

---

## Follow-up (M1–M3 complete)

M1 (TS received), M2 (full ATR), and M3 (Seedkeeper OK) are **achieved** as of 2026-03-08. See "STM32 CCID reader: full success" above. Optional follow-ups: git tag/archive, scope verification if desired, or further stress tests.

**Historical notes (pre-success):**
- **If OMNIKEY gets ATR:** Focus on STM32 activation: confirm PA4 is USART2_CK and CLKEN drives card C3 (schematic/board), I/O (PA2) correct; consider board-level fix or alternate pin if supported.
- **If OMNIKEY does not get ATR:** Focus on card/contact/reader compatibility; document before spending more time on firmware.
- **Optional firmware experiments** (if hardware suspected and no scope): e.g. log CR2/CR3 once after `set_clock(true)` to confirm CLKEN bit; or try another delay variant and re-run defmt. Each experiment: run capture → document in LEARNINGS.md → commit (e.g. "experiment: …").
  - **Done:** Log CR2/CR3 after `set_clock(true)` in `power_on()` (defmt: `CLK enabled before RST (CR2=0x…. CR3=0x…)`). Expect CR2 bit 11 (0x0800) set when CLK enabled. Re-flash and capture to confirm.
  - **Defmt finding:** CR2 was **0x3800** at runtime (bit 11 = 0) so CLKEN was not set. **Fix applied:** init now writes `cr2().write(0x3C00)`. After re-flash, defmt shows **CR2=0x3C00** at PowerOn (CLKEN=1). **Result:** Still no TS (ATR timeout). Conclusion: software CLK enable is correct; next suspect is hardware (PA4/USART2_CK not reaching card C3, or VCC/RST/I/O). See DEFMT_DIAGNOSIS activation checklist.

## M1: TS received *(achieved 2026-03-08)*

Achieved when full ATR started working after USART/activation fixes (CR2/CR3, RST sequence, tight ATR read loop). Firmware logs full ATR bytes including TS (0x3B).

## M2: Full ATR *(achieved 2026-03-08)*

Full 20-byte ATR received, T=1 detected, IFSD negotiation (IFSC=254) succeeds. PowerOn returns ATR to host; pcsc_scan shows correct ATR. See "STM32 CCID reader: full success" below.

## M3: Seedkeeper OK *(achieved 2026-03-08)*

Full pysatochip flow on STM32 reader: SELECT → GET_STATUS → Initiate secure channel → VERIFY_PIN (1234) → LIST_SECRETS → EXPORT first secret. Same outcome as Gemalto known-good. See "STM32 CCID reader: full success" below.

---

## STM32 CCID reader: full success (2026-03-08)

**Result:** With the card in the STM32 reader, `python3 test_pysatochip.py 1234 --reader OMNIKEY` (or `test_gemalto_pysatochip.py 1234 --reader OMNIKEY`) completes: ATR, SELECT SeedKeeper, GET_STATUS, Initiate secure channel, VERIFY_PIN, LIST_SECRETS, EXPORT first secret. Same outcome as Gemalto known-good.

**Two critical bugs fixed:**

1. **T=1 send sequence number (N(S)) not persisted across APDUs** — `transmit_apdu_t1` used a local `ns` reset to 0 every call; the card expects alternating 0/1. Fix: persist `t1_ns` in `SmartcardUart` ([smartcard.rs](src/smartcard.rs)) and reset on power_off; pass `&mut ns` into `transmit_apdu_t1`; increment `*ns` after each successful I-block response (non-chained and chained paths) in [t1_engine.rs](src/t1_engine.rs).

2. **I-block detection wrong for N(S)=1** — Receive path used `(pcb & 0xC0) == 0x00`, which rejects PCB=0x40 (I-block with N(S)=1). Fix: use `(pcb & 0x80) == 0` so any block with bit 7 clear is treated as I-block in the receive loop in [t1_engine.rs](src/t1_engine.rs).

**Secret:** The first secret has **label "bacon"**; the **secret data is a 24-word mnemonic** (masterseed type). Export returns full data (e.g. 244 chars hex in logs); content is not logged for security.

**Test command (remote):** With pcscd running and card in STM32 reader: `python3 test_pysatochip.py 1234 --reader OMNIKEY --log-file /tmp/stm32_test_log.txt` (script may be copied as `test_gemalto_pysatochip.py` with `--reader OMNIKEY`).

**Follow-up / Next:** M1–M3 are complete. Optional: git tag/archive, scope verification if desired, or further stress tests.
