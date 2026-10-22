# vendor-patches/ — patch-lifecycle artifacts for the #294/#379 manifest

Operational home for the artifacts behind the per-fork manifest in
`docs/vendor-patches.md` (the SSOT for pins, deltas, runtime roles,
upstream status, retirement triggers). Nothing here is a **build
input**: no `Cargo.toml` path points into this directory. These are the
series/registrations that make the manifest's "upstream status" and
"retirement trigger" columns executable rather than aspirational.

Convention — one subdirectory per patched/forked dep, only for deps
whose lifecycle needs physical artifacts:

| dir | artifact | lifecycle stage it serves |
|---|---|---|
| `synopsys-usb-otg/series/` | paste-ready upstreamable patch series (0000 cover + 0001/0002) + upstream base pin | "upstream PR of the F469 delta" (issue #271-A, owner-gated) |
| `stm32f469i-disc/` | `SNAPSHOT.md` provenance + `SHA256SUMS` integrity manifest | stranded-snapshot registration until the clean path (#265/#271-B) deletes it |

When a dep's retirement trigger fires, its directory here is deleted in
the same change as the manifest row / `[patch]` entry / vendor tree —
never left behind as a stale artifact.

## Verify

`./verify.sh` (no args: all legs) re-checks both lifecycles from a
clean state:

- `synopsys` — clones upstream `stm32-rs/synopsys-usb-otg`, checks out
  the recorded base, `git apply --check` + `git am` the series, and
  compares the resulting tree hash against the recorded fork tree
  (byte-identical ⇒ the series *is* the fork delta).
- `snapshot` — `sha256sum -c` the BSP vendor tree against
  `stm32f469i-disc/SHA256SUMS` (byte-identical ⇒ no silent drift of the
  sole-provider snapshot).

Exit code is non-zero if any leg fails. `verify.sh` is the mechanical
half of each retirement trigger: "series no longer applies onto the
recorded base" (upstream moved → rebase before sending) and "snapshot
checksums diverge" (someone edited the frozen BSP) are both CI-visible
failures, not audit findings.
