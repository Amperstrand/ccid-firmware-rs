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

## OpenPGP applet

- CAP: FluffyKaon/OpenPGP-Card `gpg.cap` (MIT), cached at
  `/root/hil-stm32-redo/j3r180-gpg/gpg.cap`,
  sha256 `5ac4b45a25a95c01548743008c18eab35368e96f94250a38ab584d8dc3f5fcdb`.
- Loads and SELECTs on the J3R180 (SW=9000, proper OpenPGP v2 FCI).
- FINDING: this JC2.2.2-era applet is NOT functionally complete on
  JCOP4 — GET DATA 6E/5E answer `6A88`, VERIFY PW1 answers `6E00`
  (loads fine, selects fine, data commands don't answer). The
  install/use/uninstall machinery is unaffected. A JCOP4-native
  applet build (e.g. JavaCardOS/OpenPGPApplet via JCIDE) is the
  follow-up for functional gpg keygen/sign.

## Results (2026-10-20, firmware e9173657 == main 6382509c content)

- ACR1252 (oracle): FULL GREEN — install/select/uninstall/baseline
  restore, org keyset SCP all pass.
- m5stick MFRC522 (our firmware): see evidence links in the PR.
- nucula c3 (our firmware): see evidence links in the PR.
