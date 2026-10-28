# Boot-smoke pre-merge gate (WS1, ccid #294)

A PR-head boot gate for the esp32 firmware: **compile-green ≠ boots** (#278
lesson). When a PR touches `Cargo.toml`/`Cargo.lock`/`sdkconfig*`/
`firmware/esp32-ccid/**` and carries the `boot-smoke` label, this gate
builds the PR head and (once the bench legs are enabled) flashes ONE
bench seat, proves the boot on the wire, and restores the known-good
image — a #278-class regression can no longer pass a green compile.

## Isolation model (why the build runs in a container)

`build.rs` of the PR under test is **untrusted code** — the same reason
`hil-bench.yml` bans PR events on the root bench runner (#183). The
build therefore runs in an ephemeral throwaway container
(`tools/boot-smoke/Dockerfile.esp32`, pinned esp toolchain mirroring
ci.yml's esp legs); the checkout is bind-mounted, caches are
bind-mounted, and **only `/out` (the .bin artifact set) crosses back**.
Bench legs check out **main only** and consume the PR's `.bin` — the PR
contributes an image, never code, to the bench host.

## What runs today vs what is inert

| Leg | State | Gate |
|---|---|---|
| `boot-smoke-build` (containerized build + artifact) | LIVE | `boot-smoke` label + diff filter |
| `boot-smoke-bench` (flash + wire probe + restore) | **INERT** | `boot-smoke-bench` label (does not exist) **and** the `bench` environment (does not exist) |

The bench leg is double-gated inert on purpose: no one — including this
workflow's author — can flash a bench DUT until an owner flips both
switches below.

## Owner enablement checklist (bench legs)

1. **Create the label** `boot-smoke-bench` in this repo.
2. **Create the protected environment** `bench` (Settings → Environments):
   required reviewers and/or a deployment branch policy. This is where
   human sign-off for pcscd-affecting automation lives.
3. **Runner**: a self-hosted runner labeled `bench` on the bench host
   with: docker, `/usr/bin/python3` + pyserial, the `NUCULA_PORT` device
   (see workflow `env`), `nucula-ccid-broker.service`, and writable
   `/tmp/amperstrand-bench.lock`.
4. **First live run = the forced-red demonstration** (card DONE MEANS (b)):
   `bash tools/boot-smoke/forced-red-fixture.sh` → push branch → PR →
   label `boot-smoke` + `boot-smoke-bench`. EXPECT RED: `i2c: CONFLICT`
   in the boot capture (the #278 signature, reintroduced by reverting
   the #292 fix), then the restore step flashing known-good with an
   uptime-monotonic probe.
5. **Record the demonstration** on the t_ae840473 card (PR link + run
   link), then the gate is live for real PRs.

## Known-good policy

`KNOWN_GOOD_IMAGE_REF` (workflow env, currently `1c05a6a36`) is the
image the restore leg rebuilds and flashes on any red/cancelled run —
a failed gate must never leave the bad image serving. Bump it only to a
bench-stamped known-good commit (docs/TESTED_AND_WORKING.md).

## Open item (owner hand)

The merge-gate paragraph for `AGENTS.md` (rule-0 family: boot-smoke red
blocks merge like CI red) is **not written** — `AGENTS.md` is
agent-write-protected; an owner adds it when the bench legs go live.
