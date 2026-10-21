### Added — NFC field-scoped locking design doc (draft proposal, t_145dec63)

- `docs/NFC-FIELD-LOCKS-DESIGN.md`: design for replacing whole-bench
  serialization with per-field locks (`/tmp/amperstrand-field-<place>.lock`)
  for operations that couple exactly one RF coil / contact slot — plus
  PERMANENT card co-location (cards fixed to specific reader coils,
  checked-in `co-location.json` + drift-detection HIL test) so field
  ownership is static. Includes the lock taxonomy (op class → lock),
  the co-location map, the staged cross-project rollout (bolty-rs /
  micronuts must adopt the same lock family before the bench flock is
  relaxed), and the explicit keep-whole-bench list (flashing, OTA,
  serial console opens, pcscd lifecycle, /dev/USB topology, power).
  New design, no prior art in-repo (searched); all verification items
  are TODO — nothing bench-proven yet, no behavior change in this PR.

### Added — raw GemPC wire/T=1 harness (gempc_t1) + pcscd strace decoder

- `tests/hardware/nfc/gempc_t1.py`: the 2026-10-20 bench-session
  scratch (t1_session/t1_verify/replay_libccid) promoted to maintained
  tooling. Importable library (stdlib-only at module scope — the
  python-collect gate imports it in a minimal env; pyserial stays
  lazy inside the hardware functions) + `--probe` CLI running the
  exact libccidtwin opening order (GetSlotStatus → IccPowerOn →
  SetParameters(T=1) → S(IFS request) → SELECT) with every frame and
  T=1 block LRC/EDC-verified. Encodes the session's hard-won facts:
  bProtocolNum=1 must ride the CCID header-tail byte 3 (else the T=1
  endpoint never arms and XFRs go out as raw TPDUs, 6700/6E00); the
  IFS request's EDC is load-bearing (no EDC → R(retransmit),
  send_ifsc stays 32); T=1 answers defer behind S(WTX request) and
  need the S(WTX response) grant dance; the device echoes frames;
  115200 8N2 with the DTR/RTS-low safe open.
- `tests/hardware/nfc/pcscd_strace_decode.py`: decodes
  `strace -f -e trace=read,write -xx -s 4096` captures of pcscd —
  pins the GemPC-talking fd BY DEVICE (/proc/<pid>/fd symlinks or
  --fd), reassembles the read stream in order, decodes all GemPC
  frames + T=1 blocks with EDC verification, and warns loudly on
  non-`-xx` captures (octal/letter escapes silently corrupt
  \f/\n/\t bytes).
- `tests/hardware/nfc/GEMPC_WIRE_HARNESS.md`: when to use the wire
  harness vs the pcscd path.
- Host-side unit locks: `test_gempc_t1.py` (frame/T=1 round-trips,
  bench-proven byte expectations, WTX dance + full setup shape against
  a scripted port, cross-check vs conformance_battery when pyserial
  exists) and `test_pcscd_strace_decode.py` (unescape/legacy-warning,
  unfinished/resumed line handling, split-read reassembly). 49 tests,
  no hardware needed. Hardware verification of the harness itself is
  the follow-up bench run.

### Added — verification digest page + honest status chapters

- `docs/TESTED_AND_WORKING.md`: the curated one-page "what works /
  what doesn't" digest — firmware × hardware × transport matrix with
  PASS/SKIP/NOT-TESTED per row, measured performance with stamps,
  the unavailable-hardware list (F469/F746 currently off the bench
  USB), and the ranked known-issues index. Derived page: every row
  cites its evidence; the generated docs/STATUS.md (bench/status)
  remains authoritative when they disagree.
- README: the stale "Status" chapter now points at the digest + the
  generated page; the "98%+ compliance" blanket claim is replaced by
  per-command coverage + per-target hardware-verification pointers.
- AGENTS.md: T=1 layer-ownership table in the Reader-vs-Standalone
  doctrine (block transport vs APDU vs secure channel are separately
  ownable layers; the host owns the SC in every reader-facing role).


### Fixed — #330: T=1 endpoint session reset

- `ccid_handler`: reset the T=1 endpoint when SetParameters(T=1) is
  accepted. A long-lived pcscd keeps the card powered between client
  connections (no power cycle → no reset anchor), so the endpoint's
  ns/host_ns counters, chain buffers and retry cache survived the
  re-arm; each new client's fresh ns=0 I-blocks then R-blocked and
  garbled applet traffic (GET DATA → 6A88 against long-lived pcscd,
  220 B on a fresh one). libccidtwin resets its counters per
  SCardConnect — the endpoint now resets at the same anchor.
  Regression: `set_parameters_resets_stale_t1_endpoint_counters`.
