# pcscd lane isolation — decision record (#545)

Status: PROPOSED (owner decision pending on the endgame; interim
formalized below). Grounded in the 2026-11-02 findings (compile-time
socket path) and the 2026-10-31 bench migration evidence.

## The constraint

pcscd's UNIX socket path is compile-time (`/run/pcscd/pcscd.comm`) —
verified via `strings /usr/sbin/pcscd`. No env/flag/config can move it.
The `/run/pcscd-lanes/*.sock` files are stale artifacts of a dead socat
bridge; a `pcscd-lane@.service` template cannot work (two instances
fight over the one hardcoded socket).

## Options

1. **Socket forwarding (socat/namespaces per lane)** — one pcscd per
   lane with a bridge onto the hardcoded path inside a namespace.
   Verdict: REJECT. Highest operational complexity (namespace + socat +
   systemd templates), and the failure mode that produced this issue
   (stale sockets, silent divergence) is intrinsic to indirection
   layers that outlive their owners.
2. **Single pcscd, all reader configs (interim — de-facto today)** —
   every lane's reader.conf lives in the main `/etc/reader.conf.d/`;
   disconnected boards simply show empty slots. Verdict: INTERIM
   FORMALIZED. Restart discipline is already solved by pcscd-ctl's
   labgrid place guard (tollgate-lab #26: denials name the holders).
   Cost: restarts remain a shared disturbance (bounded, audited).
3. **Serial-forward, pcscd at the consumer (labgrid-native endgame)** —
   the hardware-host exports `USBSerialPort` resources (already shipped
   for the small-host migration: `labgrid-exporter-small.yaml`), and
   each CONSUMING host runs its own pcscd against ser2net-forwarded
   ports. Verdict: RECOMMENDED ENDGAME. Isolation is structural (a
   lane's restart touches only its own host's daemon), it matches the
   labgrid model the bench already adopted, and it retires the
   hardcoded-socket constraint entirely (each host's pcscd uses its own
   default socket).

## Interim formalization (bench-host steps, one owner window)

- Move any lane configs still under `/etc/reader.conf.d.lanes/` into the
  main `/etc/reader.conf.d/` on each bench host (both hosts already
  list all boards — make the files match reality).
- Delete the stale `/run/pcscd-lanes/` tree.
- Update the labgrid place comments that reference the lane sockets.
- Document: lane isolation is NOW the pcscd-ctl place guard (behavioral)
  until option 3 lands structurally.

## Endgame sketch (option 3, when scheduled)

- Each hardware host: labgrid exporter with USBSerialPort (done for
  ai-legion-small; ai-legion's exporter exists for the OTA seats).
- Consumer tooling (batteries, probes) speaks the labgrid-forwarded
  serial directly or a local pcscd with a per-host reader.conf — the
  pcscd-ctl guard then guards per-host, which is the correct domain.
