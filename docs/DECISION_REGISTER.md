# Decision Register — what we chose, why, and what would change it

Companion to `DESIGN_DECISIONS.md` (CCID-protocol deviations) and the
AGENTS.md toolchain policy. Each entry names the ONE authority where the
canonical text lives, the evidence that drove it, and the concrete
trigger that should reopen the decision. If you cannot name the trigger,
you are not changing the decision — you are relitigating it.

---

## D-1: Fleet Rust toolchains — exactly three

**Authority**: AGENTS.md "Rust toolchain policy" (landed 2026-10-15, PR #201).
**Decision**: `1.99.0` exact-pinned per-repo `rust-toolchain.toml` everywhere;
CI installs read the toml. Exceptions: esp fork `1.98.1.0` (esp-idf targets),
`1.94.1` (upstream `jmcorgan/fips`'s own toml — on-demand, rustup auto-restore).
**Evidence**: bench carried 9 toolchains / 15 GB of session residue; no repo
referenced the removed ones (checked before every uninstall); upstream fips
pins 1.94.1 per their README + release notes.
**Change triggers**:
- Current stable stops building our tooling (espflash class) → deliberate bump
  of the fleet pin, full CI gate first (the 1.92→1.99 migration precedent).
- Upstream Rust gains a `-Zbuild-std`-capable stable for espidf std → retire
  the esp fork (the single largest structural exception).
- jmcorgan/fips bumps their `rust-toolchain.toml` → their new version becomes
  the aligned pin; nothing else moves.
- A repo floats (`@stable`/`@v1` in CI) → that's a regression, not a decision.
- **Toml-pinned repos MUST declare `components = ["rustfmt", "clippy"]`** in
  the toml (D-1 amendment, 2026-10-16): dtolnay's action reads components
  from the toml — not its input — on the toml-driven path; the omission
  red-lined bolty's CI (all six fleet tomls fixed the same session).

## D-2: esp-idf-svc 0.53 / hal 0.47 generation (legacy-I2C lane closed)

**Authority**: ccid + bolty `Cargo.toml` pins (unified 2026-10-15/16).
**Decision**: both ESP32 repos ride the hal-0.47/sys-0.38/svc-0.53 generation
on IDF v5.5.5, crosstool `esp-14.2.0_20260121`, fork `v1.98.1.0`.
**Evidence**: the cross-repo SDK-version matrix was the root of repeated
cmake-cache/toolchain incidents; bolty proved the 0.47 generation first.
**Change triggers**: esp-idf-svc 0.54+ requiring a new hal generation → unify
both repos again in ONE session (never one-repo-first — that divergence was
the original incident class). IDF v5.6 LTS announcements → same rule.

## D-3: BLE advertising — the 0x0c wall is CLOSED on this generation

**Authority**: issues #165/#66; captures `ble-diag-capture-2026-10-16.log`
(nucula, `AdvertisingStarted status=Success`) and `btmon-163-2026-10-16.log`
(m5stick, 501 adv reports / 25 s); PR #205 keeps the real-BtStatus surface.
**Decision**: stop treating C3 0x0c as an open blocker; the remaining BLE
work is end-to-end (connect + NUS log delivery) on BOTH boards.
**Evidence**: both boards advertise on the svc-0.53 generation; the old
failure never reproduced after the generation bump.
**Change triggers**: a regression to Command Disallowed on a future svc/hal
bump (the diagnostic board from #205 is the 10-minute reproducer); the bench
central (hci0/AX201) cannot initiate LE connections today — healthy-central
verification pending (replug/reboot, then `ble_log_capture.py`; no reflash).

## D-4: Answer-or-silence windows are per-command-class

**Authority**: `ccid_serial_server.rs` window constants (PR #204, issue #181).
**Decision**: tight 2.5 s (status/escape/mechanical), 7.5 s XfrBlock,
60 s IccPowerOn — each cited to `docs/PCSC_HOST_CONTRACT_AUDIT.md` §3.1.
**Evidence**: the flat 2.5 s starved legitimate FWI≈5 s exchanges while
libccid was still reading; host tests + m5stick battery PASS lock the table.
**Change triggers**: the host-contract audit doc gains new rows/values →
re-derive the table from it, never ad-hoc; slow-block cards measured on
m5stick hardware exceeding 7.5 s → add the MFRC522 stall-hook extension lane.

## D-5: Card-death XfrBlock failure = libccid's own convention

**Authority**: `ccid-core` handler Err arm + T=1 INF synthesis (PR #203,
issue #152). **Decision**: bError `0xF0` (card timeout) + honest dwLength=0;
T=1-armed route synthesizes SW `64 00` as I-block INF — byte-identical to
what libccid synthesizes host-side (`reference/CCID/src/commands.c` case 0xF0).
**Evidence**: GlobalPlatformPro crashed on empty INF (gp #307 class); both
frames byte-locked by host tests.
**Change triggers**: upstream libccid changes its 0xF0 synthesis → mirror it
(reference-first, single place); a host-contract finding that direct-TPDU
consumers prefer IFD_COMMUNICATION_ERROR over synthesized SW → revisit the
pipeline arm only (the T=1 arm follows the host's own engine either way).

## D-6: Device CI — nightly on the bench runner, generated status

**Authority**: issue #183 + `hil-bench.yml` + `bench/status` STATUS.md.
**Decision**: schedule/dispatch ONLY on the self-hosted root runner (public
repo — never PR events); SKIP≠FAIL; pytest-timeout `signal` method;
STATUS.md generated, never hand-written (README de-claimed).
**Change triggers**: a second bench host joins → ephemeral runners become
worth building; a month of stable nightlies → make the `hil` label a required
check for firmware/ PRs; any need to run fork code on the runner → redesign
before widening triggers (this line is a security invariant, not a config).

## D-7: Bench environment contract

**Authority**: AGENTS.md toolchain/env sections + issues #199 (closed).
**Decision**: every ESP build on .208 starts with `bash -lc` / `. ~/export-esp.sh`
(bare shells mis-resolve the esp fork); builds go through `build.sh` (sdkconfig
stamp); contaminated esp-idf-sys caches get nuked, not nursed; toolchain
auto-restore covers on-demand pins (1.94.1/1.97.1).
**Change triggers**: none for the env contract itself — violations are bugs.
The cache-zombie class (dead-worktree stamps) deserves a build.sh pre-check
refusing caches whose `esp-idf_SOURCE_DIR` no longer exists (tracked #183).

## D-8: audit-wave4 reconciliation — drop the serial consolidation, salvage the feature guards, rework frontend isolation separately

**Authority**: this entry + the preserved `sisyphus/audit-wave4` branch
(d5920a9 / c47d38b / e0c41f9, base #82 — 361 main commits past its base) +
issues #218/#219.
**Decision**: four dispositions for the unlanded branch —
- **DROP** c47d38b's UART-main consolidation (`ccid_uart_serve.rs`,
  `MalformedFramePolicy`, `PollOutcome`, thin mains): main independently
  landed BOTH headline semantics — `ServeConfig::nak_on_error` (UART NAK /
  USB-CDC drop) and `pending_notify` echo→notify→response in-band ordering —
  on a bench-verified superset the branch predates entirely (ResponseWindows
  #181, response deadlines + time-extension #156, answer-or-silence
  late-drops #153, serve-loop arrival floors). Rebasing a 528-line server
  into main's 1779-line machine is a reimplementation, not a reconciliation.
- **SUPERSEDED** (no action): d5920a9's release.yml / Dockerfile /
  check_release_config.sh / check_sdkconfigs.py + #78 console fix, and
  e0c41f9's `.omo/` gitignore — all landed via #111 and later, and evolved
  further (the release checker gained the #25 MCU/profile pairing rule; the
  sdkconfig gate gained per-family rules).
- **SALVAGED**: the NFC-backend `compile_error!` guards — backend mutex (the
  silent two-driver trap: `default` enables backend-mfrc522, so
  `--features backend-pn7160` builds two drivers), pn532⊕pn7160,
  select-a-backend, backend-pn7160 ⇒ board-nucula (arch-gated),
  pn7160-verdict pairwise mutex. Verified by a feature-matrix harness (7
  trip cases assert the messages; every build.sh/CI combo compiles clean).
- **REWORK-LATER** (tracked): frontend fault isolation → #218 (main still
  HALTS the CCID service on NFC-frontend init failure in both classic
  mains); bench-net security gate + ota.rs hardening → #219.
- `sisyphus/omo-evidence-c3-bringup`: NOT folded — its 7-line
  `sdkconfig.defaults.nucula-v2` is byte-identical to main's
  `sdkconfig.defaults.esp32c3`, and the 409-line companion patch
  self-describes as superseded; `.omo/` is gitignored by policy. The branch
  stays on origin as evidence.
**Evidence**: branch-vs-main diff at reconciliation time:
ccid_serial_server.rs 1773 diff lines (16 main commits since the branch
base), main.rs 905 (29), pn7160_ccid.rs 290 (19), nfc.rs 272 (5), lib.rs
154 (21); main's server docstrings pin the per-transport policies the
branch encoded as enums.
**Change triggers**: a bench build wedges on a dead NFC frontend → pick up
#218; an untrusted network ever sees a default build's OTA listener → pick
up #219; the per-transport malformed-frame policy ever diverges again →
revisit whether it belongs in a policy enum.

## D-9: Single I2C driver generation — driver_ng everywhere; esp-idf-hal fork retired

**Authority**: PR #242 + this entry (root `Cargo.toml` `[patch]` comment +
AGENTS.md vendoring-policy class 2).
**Decision**: every I2C consumer rides the IDF driver_ng FFI through the
shared embedded-hal adapter (`pn7160-nci::hal_i2c`); the Amperstrand
esp-idf-hal fork (`ai-experiments-0.47`, whose only delta was the
`legacy-i2c` opt-in) is RETIRED — no `[patch]` entry exists for
esp-idf-hal; it resolves from crates.io.
**Evidence**: all 3 legacy `I2cDriver` sites migrated (MFRC522 CDC main,
`wifi_common::mfrc522_bring_up`, m5stick LCD AXP192); the full-matrix CI
run on the patch-removal commit green on every esp leg (9 build legs +
`esp32-host-test`, run 39053336944 @ `3382e4e`, 2026-10-17); Cargo.lock
swapped git `c1d4ec35` → registry v0.47.0.
**Change triggers**: a future crate needing the legacy `i2c` driver API →
reintroduce via a git fork pin (vendoring-policy class 2 still applies),
never a silent re-vendor; IDF 6 removing the legacy driver entirely →
this decision becomes moot.

---

*Register discipline: new load-bearing decisions get an entry HERE with an
authority pointer and triggers; when a trigger fires, edit the entry and the
authority in the same session — the #110 rule applies to decisions too.*
