### Fixed — #332 (Codex P2s on #324): `esp32_flash.py` quiesce plumbing, wire boot-proof default, lock/marker edges

- **The nucula flash flow now passes `--quiesce-unit nucula-ccid-broker`
  itself** (was opt-in and unused by every in-repo caller): `build.sh`
  appends it for every `esp32c3` board — the standing broker owns the
  nucula CDC, and `fuser -k` alone lets systemd restart it onto the
  port mid-flash (the #314 Checksum-error class). The README manual C3
  flow documents the flag as mandatory. (`flash_and_test.sh` is
  xtensa-only — not a nucula entry point.)
- **Console-free build flows default to `--boot-check wire`**: new
  `console_none` parameter / `--console-none` CLI flag auto-defaults an
  unspecified boot check to the wire proof (a console signature can
  never appear on a `CONFIG_ESP_CONSOLE_NONE=y` build); explicit
  `--boot-check`/`--no-boot-check` still win. `build.sh` passes it for
  the `sdkconfig-xtensa.full` boards (m5stick, m5stick-plus-lcd,
  m5atom), `flash_and_test.sh` for its `sdkconfig.defaults.esp32`
  flow; `ESP_BOOT_CHECK=console|wire` overrides both.
- **The bench flock is released even if the unit restart raises**: a
  `systemctl start <quiesce-unit>` exception (e.g. a subprocess
  timeout) used to propagate out of restore() before
  `pcscd_lock_release`, stranding the held flock in a long-lived
  caller — now reported loudly (`RESTART RAISED`) and never fatal to
  the restore path.
- **The `.pcscd-down` marker is kept until pcscd actually restarts**:
  `pcscd_lock_release` used to unlink it in a `finally` even when
  `systemctl start pcscd` failed or raised — losing the only record
  that every bench reader is down. The marker is cleared only on a
  verified restart (rc=0) and the warning names the kept marker;
  `recover_stale_pcscd_down` got the same rule (a failed retry keeps
  the marker for the next attempt).

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
