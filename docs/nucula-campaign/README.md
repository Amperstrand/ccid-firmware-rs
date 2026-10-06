# Nucula CCID Campaign (October 2026) — archived working docs

Working documents from the nucula CCID bring-up campaign that produced the
ESP32-C3 / PN7160 support now on `main` (issues #62–#68). Archived as-is for
future bring-ups; this is a campaign log, not maintained documentation.

- `RUNBOOK.md` — the campaign runbook and append-only status log: hardware
  setup, build rules, phase plan, and the full debugging history (the
  pthread-stack boot-loop trap, the embuild/ldproxy build sagas, the PN7160
  VEN/I2C investigation with its verdict branches). Machine-specific details
  (hostnames, device paths, hardware serial numbers) refer to the bench of
  that campaign and are retained verbatim.
- `nfc_citation_map.md` — PN7160 / NCI 2.0 / ISO-14443 spec citation map
  (document + section reference for every driver behavior). This is the map
  behind the spec annotations in `crates/pn7160-nci` and the firmware
  PN7160 driver.
- `nfc_quick_ref.txt` — condensed citation cheat sheet for code comments.

The NXP `linux_libnfc-nci` reference clone used during the campaign is
deliberately NOT vendored into this repo; consult NXP's repository directly.
The durable facts distilled from it are captured in the citation map above.
