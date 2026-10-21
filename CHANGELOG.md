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
