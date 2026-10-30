# HIL CI practices — self-hosted GitHub runners + labgrid

Research note (2026-10-30) for the Amperstrand bench CI (boot-smoke gate, #415/#466).
Sources are cited inline; the "here" paragraphs map each practice onto our rig.

## 1. The three proven architectures

| Model | How hardware is reached | Used by | Fits us? |
|---|---|---|---|
| **Self-hosted runner on the bench host** | GitHub job runs ON the lab machine; direct serial/flash access | Mesa bare-metal farm, Konsulko (U-Boot GitLab CI) | **Yes — this is what we built** (`ai-legion-bench`, label `bench`) |
| **Federated farm with a scheduler in front** | CI submits to a device scheduler (LAVA / labgrid coordinator); runners are thin | Mesa+LAVA (ARM farms), KernelCI | **Partially** — labgrid coordinator already fronts our places; CI should go THROUGH it, not around it |
| **Runner-less tunnel** | Cloud relay (e.g. EmbeddedCI BenchPod): GitHub-hosted runner + OIDC, pod tunnels OUT to the bench | EmbeddedCI | No — adds a third party between CI and money-device hardware; noted as the pattern for distributed/external benches |

Sources: labgrid docs (https://labgrid.readthedocs.io/en/latest/usage.html),
Pengutronix labgrid (https://pengutronix.de/en/software/labgrid.html),
Mesa CI (https://deepwiki.com/bminor/mesa-mesa/6.1-continuous-integration),
Igalia "Addressing Mesa CI pain points",
EmbeddedCI (https://www.embeddedci.com/docs/benchpod-github-actions),
Konsulko (https://www.konsulko.com/year-in-review-2025).

## 2. labgrid was designed for exactly this — use its queue, not just its locks

The coordinator has first-class **reservations** for CI: a job reserves by tag filter and
WAITS instead of failing, then locks the allocated place via the token:

```
eval $(labgrid-client reserve --wait --shell 'board=nucula-c3')
labgrid-client -p + lock       # '+' expands via LG_TOKEN
... flash, test, probe ...
labgrid-client -p + release    # in an ALWAYS step / trap
```

- `--wait` queues; reservations time out if unused (self-cleaning); priorities supported.
- This is strictly better for CI than raw `acquire` (fail-on-busy) or an external flock:
  the queue is visible (`labgrid-client reservations`), host-agnostic (works from any
  runner), and integrates with the SAME locking our interactive sessions use.

**Here**: our boot-smoke bench leg takes the host flock; interactive labgrid sessions take
place locks. Two locking worlds = a race window. The bench leg should instead make a
labgrid reservation (`--wait`, bounded by the job timeout) — one queue for humans and CI
alike, and the flock stays as the last-resort guard for non-labgrid tooling.

## 3. GitHub self-hosted runner doctrine (from GitHub's own docs)

- **Ephemeral/JIT runners are the recommendation** (`config.sh --ephemeral` or JIT
  registration API): one job per runner instance, auto-deregistered, clean environment each
  time. Persistent runners are explicitly "not recommended" because a compromised runner
  keeps receiving jobs.
- **Never for public repos / fork PRs**: anyone who can trigger a workflow can compromise
  the runner host and its secrets. Private repo + same-repo branches only; our label-gated
  trigger (`boot-smoke` + `boot-smoke-bench`, maintainers-only labels) already enforces this.
- **Minimize what lives on the runner host**; treat it as shared state. Forward runner logs
  externally for forensics.

**Here**: a bench runner is INHERENTLY persistent (hardware doesn't ephemeralize), so
compensate: dedicated host or dedicated user/container for the runner service; the `bench`
label routes only approved jobs; secrets scoped to the `bench` environment only (never
org-wide); environment protection rules = the human approval gate. That is exactly the
checklist in docs/BOOT-SMOKE-GATE.md — this research confirms the design.

Source: https://docs.github.com/en/actions/reference/runners/self-hosted-runners and
https://docs.github.com/en/actions/reference/security/secure-use

## 4. Mesa farm wisdom (the largest public hardware-CI operation)

1. **Stage the pipeline: hardware LAST.** sanity → build → static/unit → hardware tests,
   so boards only see changes that already passed everything cheap. (Our paths-filter +
   ci-run label ordering already does this; keep it.)
2. **Farm kill-switch.** A marker file (`.ci-farms-disabled/<farm>`) drops a whole farm
   from CI during maintenance without workflow edits. Cheap, operator-friendly, prevents
   red-noise during bench work. **Adopt**: a `BENCH_CI_DISABLED` marker checked by the
   boot-smoke bench leg (skip-with-notice, not fail).
3. **Expectation files: fails / flakes / skips per device.** Hardware is noisy; instead of
   demanding 100% green, keep per-device lists: expected-fails (CI alerts if one starts
   PASSING — keeps the list honest), flakes (retry + ignore intermittent), skips (too
   slow/unstable). **Adopt once** the conformance battery runs in CI: a `ci/expectations/`
   dir with nucula/m5stick fail+flake lists, and the required check compares against them.
4. **FRACTIONAL runs per-change, full runs nightly.** Per-MR jobs run a subset
   (deqp `--fraction`); complete suites run on schedule. **Adopt**: PR boot-smoke = flash +
   boot + CCID echo (~minutes); full conformance + OTA proofs + soaks = nightly/scheduled on
   main.
5. **Timeouts everywhere**: 1h per MR hard cap; jobs killed release their boards.
6. **Be kind to shared farms**: serialize merges through a queue (Mesa's Marge bot) so
   hardware tests don't stampede; delay stress/soak jobs to off-hours. **Here**: GitHub
   environment `concurrency` already serializes bench jobs; keep soak/nightly on schedule,
   never on PRs.

## 5. The trust anchors (both non-negotiable, both already ours)

- **Forced-red demonstration**: a gate that has never failed is decoration — the broken
  fixture run (#278 i2c-CONFLICT signature) proves the veto works end-to-end. Pending on
  the runner checklist (#466).
- **KNOWN_GOOD restore + uptime-monotonic probe after every flash**: the bench is never
  left on a PR image; the restore itself is verified. (Landed in #415.)

## 6. Actionable deltas for our rig (ranked)

1. Bench leg acquires via labgrid reservation (`reserve --wait --shell` + `-p +` lock,
   release in an `if: always()` step) instead of the flock alone. — small PR
2. `BENCH_CI_DISABLED` marker file kill-switch checked by the bench leg. — small PR
3. Forced-red demonstration once the runner is exercised (#466 DONE MEANS (b)). — bench
4. Per-device expectation lists + fractional-vs-nightly split when the conformance battery
   enters CI. — with #468's re-stamp work
5. Runner hardening pass: dedicated service user, `bench`-environment-scoped secrets only,
   external runner-log forwarding, label audit. — ops
6. Optional later: dedicated second runner host for the m5stick lane so c3 and xtensa legs
   parallelize (environment concurrency: per-board groups instead of global).

## 7. What we deliberately do NOT adopt

- eFuse/secure-version anything on CI-flashed units (D-13: recovery stays open).
- Public-repo or fork-accessible bench triggers.
- Cloud-relay HIL (third party in the path to money-device hardware).
