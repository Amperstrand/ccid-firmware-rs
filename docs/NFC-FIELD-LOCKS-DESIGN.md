# NFC field locking — permanent co-location + field-scoped locks

**status: draft proposal (new design, no prior art in-repo)**

- Kanban: `t_145dec63` "NFC field locking — permanent co-location +
  field-scoped locks (replaces whole-bench serialization)"
- Proposed: 2026-10-21. Owner review pending — nothing here is merged
  doctrine until the migration section is executed and verified.
- Provenance: a search for existing design material
  (`*field*lock*` / `*FIELD-LOCK*` across this repo, bolty-rs,
  `/tmp/opencode`, `/root/handoffs`; grep of AGENTS.md for "field lock")
  found **nothing** — this doc is a new proposal, not a promotion. Every
  claim about current bench behavior below is grounded in the cited
  in-repo sources; every claim about the new scheme is a **TODO until
  bench-verified**.

## 1. The problem: whole-bench serialization for single-field work

Today the bench's exclusivity is three layers, taken in this order
(`tests/hardware/labgrid/bench.py` header, mirrored in
`tests/hardware/labgrid/README.md` §"Bench coordination"):

1. **Bench flock** on `/tmp/amperstrand-bench.lock` (`bench.bench_lock`)
   — the ONLY layer that excludes same-user sessions of the other
   projects sharing the bench (bolty-rs, micronuts).
2. **labgrid place acquisition** — per-place exclusion between labgrid
   sessions.
3. Read-only pcscd state needs neither, but must not run while another
   holder toggles pcscd.

The granularity mismatch: `bench_lock` is **session-scoped and
bench-wide**. The fixture table (`tests/hardware/labgrid/README.md`)
says place-aware tests request `bench_lock` "when they drive pcscd or a
serial port" — so a single-reader card session (one APDU differential
leg through the ACR1252 oracle, one T=1 probe on the m5stick coil)
blocks *every* other ccid/bolty/micronuts session on the bench, even
ones touching entirely different devices.

Contention evidence (all in-repo):

- The bench is explicitly shared: "the bench is shared with
  micronuts/bolty-rs, and the m5stick was re-flashed by a micronuts
  session within ~30 min" (AGENTS.md, Known-Good Bench State preamble).
- Live sibling contention: "The sibling root session on .208 actively
  holds `nucula-c3` + `m5stick`" (AGENTS.md, bench state delta
  2026-10-14).
- pcscd is a reader-set-wide coupling: any stop/start must serialize on
  `flock /run/lock/bench-pcscd.lock`, and the conformance battery stops
  pcscd, so "no other test can hold it during a battery run" (AGENTS.md,
  "Bench coordination rules"). A reader.conf flip costs a pcscd restart
  (`mv` + `flock /run/lock/bench-pcscd.lock systemctl restart pcscd`,
  AGENTS.md nucula row).
- Card-location drift blocks real work: "CAP install still PENDING a
  CARD MOVE: the keyed card 50942720 … sits on the ACR1252 coil; the
  m5stick coil holds [a different card]" (AGENTS.md m5stick row) — and
  the repo's two card tables themselves disagree on where 50942720
  lives (AGENTS.md vs `tests/hardware/labgrid/README.md` card table),
  which is the drift class this design exists to kill.
- Host-side reader-set coupling is real but per-reader connection
  scoped: "scdaemon wedges uninterruptibly on the host's shared reader
  set" (CHANGELOG [Unreleased], J3R180 findings) — solved by pinning
  readers, not by bench-wide exclusion.

Meanwhile the bench has already PROVEN that independent readers serve
concurrently: "all five project readers serve pcscd SIMULTANEOUSLY"
(AGENTS.md, 2026-10-12 session verification). Concurrency is not the
risk; the risk classes are (a) one physical RF field / contact slot
coupling one card, (b) USB/serial port side effects, (c) pcscd daemon
state, (d) flashing. Only (a) is per-reader — (b)–(d) stay serialized.

## 2. Design goal (from the kanban title)

1. **Permanent co-location**: each test card is physically fixed to one
   reader coil / contact slot (holder or tape), so *field ownership is
   static* — "which card is coupled to which device" becomes an
   invariant, not a per-session discovery.
2. **Field-scoped locks**: single-field operations take
   `/tmp/amperstrand-field-<place>.lock` instead of the whole-bench
   flock, so sessions on disjoint fields run concurrently — while
   flashing, USB, power, and pcscd-lifecycle operations keep the
   whole-bench lock.

## 3. Coupling domains

| Domain | What couples | Scope |
|---|---|---|
| Field (RF coil) or contact slot | One reader's field energizes / answers exactly the card on it; two different readers' fields never share a card | per-reader |
| USB / serial port | Port opens toggle DTR/RTS (the nucula CDC open chip-resets it — AGENTS.md; ser2net opens on place acquire — `tools/hil/README.md` §3); ST-LINK, replug, re-enum | per-device, but with host-wide side effects (udev, pcscd hotplug) |
| pcscd daemon | Stop/start/restart bounces every reader's connections; reader.conf changes require restart | bench host (.208) |
| Physical bench | Power, cabling, exporter/coordinator deploys | whole bench |

The design decomposes the lock graph along exactly these lines.

## 4. Lock taxonomy (op class → lock)

Acquisition order is always top-down; a field lock is a **leaf** — never
acquire any other lock while holding one (preserves the existing AB-BA
discipline of `bench.py` / bolty-rs `docs/labgrid-bench-sharing.md`).

| # | Op class | Examples (in-repo callers) | Lock(s) |
|---|---|---|---|
| 1 | Single-field card ops (no pcscd lifecycle) | `tests/hardware/nfc/turnaround_probe.py`, `t1_retry_probe.py`, one differential leg (`test_differential.py`), Escape 0xD0/0xD2/0xD3 probes | **field lock** for that place → labgrid place |
| 2 | Single-field pcscd-serving ops (connection opens, GP/gpg flows) | `test_gpg.py` leg, `j3r180_gpg_lifecycle.py` on one reader | **field lock** → labgrid place (pcscd stays up; serving is per-reader — proven, §1) |
| 3 | Multi-field sequential ops | full differential run (same card through every reader — sequential by design) | field locks acquired **one at a time**, released between seats; never hold two |

**Differential seat (review P2):** the full differential's premise — ONE card through EVERY reader — is incompatible with permanently fixed per-reader cards. Keep a dedicated **rotation card** (its own labeled seat, CPLC-ledgered) that is the ONLY card allowed to move between fields; per-reader resident cards stay fixed. The differential run then uses the rotation card seat-to-seat (field lock per seat, one at a time), and the drift-detection HIL asserts both the fixed map AND the rotation ledger. Cost: the differential no longer exercises the *resident* cards — acceptable because residents exist to keep each reader's field deterministic, while card-variance questions (#327-class) are exactly what the rotation card exists to answer.
| 4 | pcscd lifecycle | conformance battery (`conformance_battery.py` stops pcscd), reader.conf enable/disable + restart | **whole-bench** + `/run/lock/bench-pcscd.lock` (existing rule) |
| 5 | Flashing / OTA / console | `esp32_flash.py`, `stm32_flash.py`, `host-tools/ota_push.py`, coredump trigger/retrieve/erase, any serial open | **whole-bench** → labgrid place |
| 6 | /dev or USB topology | replug, ST-LINK ops, exporter/coordinator restart or config deploy, udev work | **whole-bench** |
| 7 | Power ops | board power-cycle, bench PDU | **whole-bench** |

Rule of thumb: **if the op couples exactly one field and nothing else,
it takes one field lock. Everything else takes the bench.**

## 5. Co-location map (target state)

Card IDs are CPLC serials (stable); ATR is *negotiated per reader from
the ATS* (J3R180 runbook) so ATR alone must never identify a card.
Verification probe per seat: card-present via Escape 0xD0 `present` flag
+ CPLC read — both already exist as bench patterns (AGENTS.md,
"Bench coordination rules"; #127 card table).

| Place (exporter-ai-legion-nfc.yaml) | Coupling domain | Card fixed there (CPLC) | Notes |
|---|---|---|---|
| `ref-acr1252` | RF field (PICC) | **TODO** — README says 50947320, AGENTS.md (older) says 50942720; reconcile at fixation time | working AUTH triple card must land on ONE seat and stay |
| `m5stick` | RF field (MFRC522) | **TODO** — README says 50861320, AGENTS.md (older) says 50906420 | the "CAP install pending card move" blocker (§1) is the first payoff |
| `nucula-c3` | RF field (PN7160) | unprobed (retry-budget discipline, #127) — **TODO** | probe once, then fix |
| `stm32-ccid` (F469 Cherry) | contact slot | **TODO** — README says 50942720 (Cherry contact) | contact slots get the same lock semantics: one slot couples one card |
| `stm32-f746` | contact slot (bitbang) | **TODO** | currently phantom (user USB unplugged) |
| `ref-cardman` | contact slot | **TODO** — README says 50972320 | reference oracle, never a DUT |

The authoritative map lands as a checked-in file (proposed:
`tests/hardware/labgrid/co-location.json`, one entry per seat: place,
card CPLC, expected ATR family, probe) plus a **drift-detection HIL
test** that fails red when the physical world disagrees with the file —
that test is what makes co-location "permanent" instead of a stale
table. The README-vs-AGENTS.md disagreement in §1 is precisely the
failure this replaces. (TODO: pick the CPLC source of truth per seat
before fixation; the two current tables disagree, so at least one is
stale.)

## 6. Lock protocol details

- **Path scheme**: `/tmp/amperstrand-field-<place>.lock`, one per place
  in `bench.PLACES` (+ `stm32-f746`). Matches the existing
  `/tmp/amperstrand-bench.lock` convention; `/tmp` lifetime semantics
  are inherited and accepted exactly as the bench lock already accepts
  them.
- **Semantics**: same as `bench.bench_lock` — non-blocking by default,
  `LOCK_EX | LOCK_NB`, optional timeout polling at 1 s, raises a
  dedicated `FieldLockHeld` naming place + holder intent; HIL legs
  **SKIP with the holder named** (mirrors the place-busy and SKIP≠FAIL
  #183 doctrines), never fail red on contention.
- **Holder visibility**: lock files are opaque to flock; the run ledger
  (`results/history.jsonl`, existing) gains an event on acquire/release
  so `bench_inventory.py`-style tooling can show field-lock holders.
  TODO if insufficient: a `<path>.info` sidecar written best-effort.
- **No nested field locks**: classes 1–3 only ever hold one field lock;
  if a future op truly needs two, acquire in sorted place order and
  document it here first.
- **Whole-bench ops ignore field locks** (class 4–7): the bench lock
  outranks everything by protocol, and those ops assume bench-wide
  exclusivity anyway — including against field holders (a pcscd restart
  or a flash legitimately stomps an in-flight field session; the
  field holder's guard is that pcscd/flash ops are advertised in the
  ledger and, per class 4–7, may only run under the bench lock).

## 7. What still needs the whole-bench lock (explicit list)

1. **Flashing, any flavor**: `esp32_flash.py`, `stm32_flash.py`
   (connect-under-reset), OTA pushes (`host-tools/ota_push.py`, places
   `ccid-nucula` / `ccid-m5stick`), bootloader/PT/app partitions.
2. **Serial console opens** — DTR/RTS side effects chip-reset boards
   (nucula CDC hazard; ser2net-on-acquire hazard), including coredump
   trigger/retrieve/erase and any `pyserial` open of a console port.
3. **pcscd stop/start/restart** and reader.conf enable/disable — the
   conformance battery, `flock /run/lock/bench-pcscd.lock systemctl
   restart pcscd` flows (existing rule unchanged).
4. **Anything touching `/dev` or USB**: replug, ST-LINK use, USB PHY
   resets, udev rules, exporter/coordinator restarts and config
   deploys (`/etc/labgrid/*.yaml`), by-id topology changes.
5. **Power ops**: board power-cycles, bench PDU switching.
6. **Labgrid place topology edits** (place creation/matching changes,
   `tools/hil/labgrid-place.sh` runs).

## 8. Cross-project rollout (staged — siblings must adopt)

The bench flock exists *because* bolty-rs/micronuts sessions share this
bench and are not all labgrid-disciplined. Field locks only decompose
exclusion safely if the sibling projects recognize the same paths:

- **Phase 1 (this repo, no exclusion lost)**: add `field_lock()` to
  `bench.py`; single-field ops take field lock **and** still take the
  bench lock. Pure observability + plumbing; contention relief ≈ 0.
- **Phase 2 (docs + siblings)**: update the cross-project canon —
  bolty-rs `docs/labgrid-bench-sharing.md` and the conwrt-bench
  `docs/labgrid-coordination.md` (org-wide canon per the
  labgrid README header) — to define the field-lock family and the
  "leaf lock, bench lock outranks" protocol.
- **Phase 3 (relax)**: ccid single-field ops drop the bench-lock
  requirement once no sibling session can be surprised — i.e. after
  bolty/micronuts tooling either takes field locks for their own
  single-field work or is confirmed labgrid-place-only. Per-place
  labgrid exclusion (coordinator-enforced, verified 2026-10-14) remains
  the belt-and-braces layer against non-adopting siblings during the
  transition.
- **Phase 4 (default flip + doctrine)**: flip the `bench_lock` fixture
  default off for classes 1–3; update AGENTS.md ("Bench coordination
  rules", "Testing through labgrid") and both READMEs.

## 9. Verification plan — ALL TODO (design only, nothing bench-proven yet)

- [ ] Fix cards physically; record per-seat CPLC via the #127
      plain GET DATA CPLC (`80 CA 9F 7F 00` — no secure channel, no
      auth budget; bench-proven 2026-10-21 via the field recorder:
      full BER-TLV response + 9000 in 17 ms). NEVER raw INITIALIZE
      UPDATE — it is SC-class and burns auth budget (review P1);
      land `co-location.json`.
- [ ] Drift test: HIL leg asserting each seat's co-location entry
      (present flag + CPLC) — red on any moved/swapped card.
- [ ] Concurrency soak: two sessions on disjoint fields (e.g. m5stick
      APDU hammer + nucula differential leg) — zero cross-interference,
      zero lock errors, results identical to serialized runs.
- [ ] Exclusion check: two sessions on the SAME field — second skips
      with holder named, first unaffected.
- [ ] Class 4–7 guard: pcscd restart under bench lock while a field
      session runs — verify the field session's failure mode is a clean
      skip/error, not a wedge (scdaemon shared-reader-set lesson).
- [ ] Sibling dry-run: a bolty/micronuts-shaped session (bench lock
      only) vs a ccid field session — confirm the Phase-3 safety
      argument on the real bench.
- [ ] Nightly: hil-bench nightly green for a full cycle after Phase 3.

## 10. Migration checklist (concrete, doc → code)

1. This PR: design doc + CHANGELOG only. No behavior change.
2. Physical fixation + `co-location.json` + drift test (its own PR).
3. `bench.py`: `FIELD_LOCK_PATHS`, `field_lock(place, timeout_s=0.0)`,
   `FieldLockHeld`; ledger events; conftest fixture. (Phase 1)
4. Opt-in per suite: classes 1–3 callers add the field lock (Phase 1).
5. Canon updates in bolty-rs / conwrt-bench (Phase 2).
6. Relax + flip + AGENTS.md doctrine rewrite (Phases 3–4), gated on §9.

## References

- `tests/hardware/labgrid/bench.py` — current three-layer protocol,
  `bench_lock` (lines 64–91), `PLACES`.
- `tests/hardware/labgrid/README.md` — bench coordination, fixture
  table, place topology, bench cards & GP table (#127).
- `tests/hardware/labgrid/exporter-ai-legion-nfc.yaml` — the seats and
  their identities (source of the place list; `docs/device-inventory.json`
  does not exist on main as of 2026-10-21).
- `AGENTS.md` — "Known-Good Bench State" (+ bench state deltas
  2026-10-14/15/17), "Testing through labgrid" bench coordination
  rules, pcscd serialization rule, m5stick card-move blocker.
- `tools/hil/README.md`, `tools/hil/labgrid-exporter.yaml` — flock-first
  rule, DTR hazard, cross-place coordinator-enforced exclusion.
- `CHANGELOG.md` [Unreleased] — J3R180 reader-set/scdaemon coupling
  finding.
