# J3R180 JavaCard — known-good install/use/test runbook

Established 2026-10-20 on the ai-legion bench. The J3R180 cards are
the org's dev JavaCards (JCOP4). This is the proven-safe envelope for
carrying a full GlobalPlatform + OpenPGP lifecycle through ANY
pcscd-visible reader on the bench.

## Keys (single source of truth)

The org dev keyset — history in `tests/soak/soak_02_globalplatform.py`
(the old JR3180 rig); now pinned here and in the lifecycle tool:

```
--key-enc 5A9E63D03BADBC2A240FE8F534709EDF
--key-mac 7CCC1E79D64FC5FA263B8F2955282998
--key-dek B040703EC3DE23EE8AE4CFB6D632AA80
```

NEVER let gp fall back to default keys against these cards: a failed
SCP cryptogram burns auth budget and gp itself warns that re-tries can
brick the card. One default-key attempt was made 2026-10-20 before the
org keyset was located — card unaffected, but the class is real.

## The lifecycle tool (canonical path)

```
python3 tests/hardware/nfc/j3r180_gpg_lifecycle.py \
    --reader "M5stick GemPC (MFRC522) 00 00"        # or ACR1252 / nucula
```

Runs: preflight → cleanup (idempotent) → baseline → install → SELECT →
applet-function findings → bounded gpg probe → uninstall →
baseline-restore verification. Exit 0 = every contract phase green.

Its preflights encode every trap of the establishing session:

- `scdaemon.conf` must not pin `reader-port` (the old soak rig left
  `reader-port 4` — every bare `gpg` invocation died on a reader index
  that no longer existed).
- gpg is driven ONLY through `SCD … --reader=N` with N resolved fresh
  from pcscd; never bare `gpg --card-status` (scdaemon auto-scans ALL
  readers — this host's pcscd carries vpcd virtual PICCs and a remote
  modem IFD for other lanes, and scdaemon wedges UNINTERRUPTIBLY on
  them; the tool SIGKILLs and reports the wedge class as DEGRADED).
- ATR wrong-card guard accepts the J3R180 ATR FAMILY — the ATR is
  negotiated per-reader from the ATS: `3B 85 80 01 80 …` via ACR1252,
  `3B 85 81 31 FE 45 …` via the m5stick MFRC522. Same card.
- ISD must authenticate with the org keyset before anything else.

## Reader matrix (how each reader becomes a pcscd reader)

| Reader | pcscd config | Notes |
|---|---|---|
| ACR1252 oracle | USB CCID (automatic) | the reference reader |
| m5stick (our fw) | `/etc/reader.conf.d/m5stick-gempc` — `libccidtwin.so` on the by-id port, `:GemPCTwin` | our firmware impersonates the GemPC Twin serial protocol; pcscd opens reset the board once (DTR auto-reset), it re-serves in ~10 s |
| nucula c3 (our fw) | `/etc/reader.conf.d/nucula-broker` (pre-existing) — `libccidtwin.so` on `/dev/nucula-ccid:GemPCTwin` | requires the `c3-ccid` serving image (NOT `build.sh c3` bringup — that image ladders the card itself and serves no CCID) + `systemctl start nucula-ccid-broker` (issue #117 PTY relay), then a pcscd restart |
| F469 (our fw) | USB CCID (automatic) | contact slot — ComSign eID lives there, not a J3R180 |

gp syntax note: GlobalPlatformPro uses `--delete <AID>` /
`--uninstall <cap>` — the GPShell-style `-u -d` is silently rejected.
Uninstall takes the INSTANCE AID (`gp -l` `Applet:` lines), not the
package AID (`PKG:` lines).

## OpenPGP applet — the proven recipe

- **CAP: SmartPGP v1.23.0, javacard 3.0.4, RSA to 4096** — the
  gpg-proven applet of the wifi-vpcd e2e suite. Bench path (caps are
  never committed): `/root/src/ccid-firmware-rs/.omo/gp-artifacts/
  SmartPGP-v1.23.0-javacard-3.0.4/SmartPGPApplet-rsa_up_to_4096.cap`,
  sha256 `9213b1d6d53764b00b0752028b491971e1b210c768643434fc05de82acba7422`.
- **`gp --bs 200` is MANDATORY**: APDUs >~245B fail SW 6400 on this
  card path; without it gp's oversized blocks fail and retry at a
  crawl. This single flag took the serial install from >480s-timeout
  to 35-65s COMPLETE. Install budget 900s (117KB CAP ≈ 4 min over
  vpcd per the e2e suite).
- The 9KB FluffyKaon ss3t CAP (JC2.2.2 era) loads and SELECTs on
  JCOP4 but answers 6A88/6E00 to data commands — superseded by
  SmartPGP; kept as documented fallback only.

## Results (2026-10-20, firmware e9173657 == main 6382509c content)

| Reader (path) | install | SELECT | applet data ops | uninstall | baseline |
|---|---|---|---|---|---|
| ACR1252 oracle (USB CCID) | ✅ 16s | ✅ | ❌ 6A88 (card) | ✅ | ✅ 4 pkgs |
| nucula c3-ccid (CDC→broker→pcscd) | ✅ 65s | ✅ | ✅ **FULL** (6E 220B, PW1 9000, 5E) | ✅ | ✅ EXIT=0 |
| m5stick MFRC522 (GemPC serial) | ✅ 35s | ✅ | ❌ 6A88 (card) | ✅ | ✅ after IFD restart |
| F469 (USB CCID, contact slot) | pending physical card move | | | | |

- **The nucula serial path is the fully-proven reader**: complete
  install/use/uninstall of the gpg-grade applet, clean baseline
  restore, our firmware end to end.
- FINDING (card variance, not reader): SmartPGP data commands answer
  6A88/6E00 on the ACR-coil and m5stick-coil cards while the SAME
  applet is fully functional through the same lifecycle on the
  nucula-coil card (the card that shipped SmartPGP v1.23.2.0
  preloaded). Reader-independent → card-state/batch investigation
  filed.
- FINDING (m5stick pcscd-IFD class): under heavy phases the
  libccidtwin IFD wedges (SCardConnect hangs; firmware stays alive —
  raw GetSlotStatus instant). Recovery: pcscd restart under the bench
  flock. After ANY pcscd restart the serial readers may need one more
  restart once the boards settle (init race, RFAddReader window).
- FINDING (wifi-vpcd regression): the reversed-vpcd WiFi path (proven
  2026-10-13, runbook + e2e suite) no longer completes the ATR
  exchange — the board boots, joins WiFi, PN7160 inits (attempt 1),
  connects to the host listener, but SCardConnect hangs waiting for
  the ATR answer. First run since the driver_ng migration era
  (#242/#321). Firmware-side debug filed.
- gpg/scdaemon on this host stays DEGRADED-by-design in the tool
  (reader-set wedge class) — card ops ride direct APDUs; the gpg leg
  is a bounded probe.
