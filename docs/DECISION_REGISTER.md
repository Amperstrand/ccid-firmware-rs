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
- A repo floats its RUST COMPILER input (dtolnay `@master`/`@stable`/unpinned
  toolchain, or an unpinned espup) → that's a regression, not a decision
  (approved *action* pins like `setup-rust-toolchain@v1` are fine — the
  trigger is the compiler, not the action tag).
- **Toml-pinned repos MUST declare `components = ["rustfmt", "clippy"]`** in
  the toml (D-1 amendment, 2026-10-16): dtolnay's action reads components
  from the toml — not its input — on the toml-driven path; the omission
  red-lined bolty's CI (all six fleet tomls fixed the same session).

## D-2: esp-idf-svc 0.53 / hal 0.47 generation (legacy-I2C lane closed)

**Authority**: ccid + bolty `Cargo.toml` pins (unified 2026-10-15/16).
Fork-pin map per dep: `docs/vendor-patches.md` (#294 manifest).
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
59.5 s IccPowerOn (ICC_POWER_ON_RESPONSE_WINDOW_MS = 59_500 — the 500 ms
host-contract margin is load-bearing) — each cited to
`docs/PCSC_HOST_CONTRACT_AUDIT.md` §3.1.
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
AGENTS.md vendoring-policy class 2). Every patched/forked dep's pin, fork
delta, runtime role, upstream status and retirement trigger are manifest-ed
in `docs/vendor-patches.md` (#294 item 3 — the WS3 vendor-patch manifest).
**Decision**: every embedded-hal I2C consumer rides the shared adapter
(`pn7160-nci::hal_i2c`); the PN7160 transport itself rides the raw
driver_ng FFI directly by design (esp_transport.rs); the Amperstrand
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
never a silent re-vendor — and the new fork gets a manifest row (pin, delta,
runtime role, upstream status, retirement trigger) in `docs/vendor-patches.md`
the same session; IDF 6 removing the legacy driver entirely →
this decision becomes moot.

**Trigger FIRED and RESOLVED (2026-10-18, #278)**: registry hal links
IDF's legacy i2c object, whose boot-time ELF constructor `abort()`s when
driver_ng is also in the binary — every c3-ccid image from `3382e4e`
boot-looped before `app_main()` while the full compile CI matrix stayed
green (caught only by #273's hardware bisection: compile parity is NOT
boot parity). Resolution: `CONFIG_I2C_SKIP_LEGACY_CONFLICT_CHECK=y` in
every authoritative `.full` sdkconfig + defaults — IDF's own documented
bypass (`components/driver/Kconfig`; the symbol #242 called non-existent
after a repo-scoped grep — it lives in IDF's Kconfig, not ours). The
fork's real runtime role was keeping the legacy object OUT of the link;
that role is now the knob's. Bench-proven on nucula-c3: clean `app_main`
+ pcscd absent-card round-trip through the broker. Layering lesson:
`sdkconfig.defaults` does NOT merge into an existing `.full` config.

## D-10: nucula-board upstream posture — consume, don't fork (O-1)

**Authority**: `docs/BOARDS-AS-DATA-DESIGN.md` §2.1 (design merged via #396);
this entry records the open decision as resolved by the implementation card
(t_099ed2af pilot).
**Decision**: ccid reads `zeugmaster/nucula-board` as a read-only dependency
at *generation time only* (pinned rev + netlist sha256 recorded in the
generated `crates/boards/src/nucula_gen.rs` header); the net-name sidecar map
(`crates/boards/gen/nucula-nets.toml`) lives in ccid. No Amperstrand fork of
the CAD repo.
**Evidence**: upstream is alive (last push 2026-10-09), CERN-OHL-S-2.0
permits use; a fork's PR-back workflow adds nothing under the EXTERNAL-POST
GATE (ops doctrine, 2026-10-21 — no automated interaction with non-org
repos); the generator + sidecar make either posture a one-line repoint.
**Change triggers**: we ship our own schematic change ("nucula all-in-one"
product milestone) or upstream goes dormant while rev-A/B units are in
service → fork `Amperstrand/nucula-board` and repoint the generator.

## D-11: boards crate name — `boards` (O-2)

**Authority**: `docs/BOARDS-AS-DATA-DESIGN.md` §1.4; crates/boards landed by
t_099ed2af.
**Decision**: the workspace-level board-table crate is named `boards`
(candidates were `board-registry`, `ccid-boards`). The pn7160-nci precedent
keeps block-scoped names (`board.rs` inside a crate); a whole-board registry
claims the short name.
**Evidence**: the crate is repo-internal (path dep) until bolty consumes it
via a rev-pinned git dep — at which point it gains a `docs/SHARED-CRATES.md`
row + drift-check entry (design §3.4). The short name is what that row wants.
**Change triggers**: a crates.io publish or a name collision in bolty's
workspace → rename is cheap while path-dep-only (one workspace line per
consumer); after the bolty flip it needs a coordinated rev bump.

## D-12: board selection stays Cargo features; presets compose on top (O-3)

**Authority**: `docs/BOARDS-AS-DATA-DESIGN.md` §3.1/§3.3 (O-3).
**Decision**: as-designed for now — per-board cargo features
(`board-m5stick` …) remain the compile-time selector; a feature selects a
`boards::ALL_BOARDS` table entry instead of carrying pin data inline. Product
presets (M3, `cargo xtask build product=X`) layer on as wants ⊆ haves
resolution, not as a replacement of features.
**Evidence**: `#![no_std]` + dead-code elimination + the existing
mutually-exclusive-feature contract (`compile_error!` guards) all ride
features; the design supports flipping later because the registry, not the
feature list, holds the data.
**Change triggers**: M3 presets landing (t_506dfe03 milestone) → revisit
whether presets replace per-board features entirely (registry selected by
preset name). Decision point is M3, not before.

---

*Register discipline: new load-bearing decisions get an entry HERE with an
authority pointer and triggers; when a trigger fires, edit the entry and the
authority in the same session — the #110 rule applies to decisions too.*

## D-13: dev-fleet OTA signing — Ed25519 software chain, dual-host keys, recovery never locked

**Authority**: owner directive 2026-10-30 on #494's open decision (field-tier
OTA signature scheme).

**Decision**:
1. **Ed25519 software verification** is the fleet signature scheme — bolty's
   existing verify-before-commit chain (`docs/BOOTLOADER_OTA_PRACTICES.md`
   §2/§3) is the reference implementation; ccid's OTA2 32-byte token remains
   the bench floor until its signature upgrade (the audit's M-gap "before
   field exposure").
2. **Keys we control, redundantly available**: the private key lives on BOTH
   ai-legion and ai-legion-small — either host alone can sign, so no single
   host failure locks us out. Locations: `/root/src/keys/ota-field-ed25519.{key,pub}`
   (key mode 0600; sha256 `93c45232596f99af…` / pub `e2d87585e5da55c0…`).
   ai-legion-small copy DONE (2026-10-30): the "dark host" was a dead
   `proxyjump openwrt` (WAN hop) in ai-legion's ssh config — the direct
   LAN route works and is now the config; key+pub installed at
   /root/src/keys/ with sha256 fingerprints verified identical on both hosts.
3. **HARD NO — lockout class**: NO eFuse secure boot, NO secure_version /
   anti-rollback burning, NO mechanism that disables USB/serial recovery.
   These are development units, not production: recovery must stay open
   unconditionally. `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` is allowed (a
   software slot-revert; serial flash always recovers) — key-escrow lockouts
   are not.

**Evidence**: audit §4 (anti-rollback eFuses OFF on bench already);
bolty Ed25519 chain in production use on HTTP-pull; ccid #461 serve-proven
mark-valid as the self-test half of the rollback contract.

**Change triggers**: a production (non-dev) hardware tier existing → reopen
secure-boot and anti-rollback for THAT tier only, with a key ceremony and
escrow; dev units keep this decision unchanged.
