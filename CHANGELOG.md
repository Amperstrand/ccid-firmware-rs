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
