> **Scope:** weekly / on-demand virtual-card regression — the #261 ladder
> (tier-a table card, tier-b jcardsim bridge) exercised against a real
> pcscd + patched libifdvpcd on a stock GitHub runner. Never runs on PRs
> (#367 opt-in policy keeps PR cost zero); this file is the runner-side
> recipe and the bench-side recipe is the test module docstring.

# Virtual-card CI (vjavacard)

Runs the `tests/hardware/host/test_vjavacard.py` suite against a
fully virtual reader stack built from scratch on the runner:

    pcscd → libifdvpcd (patched, #326/#359) → vpcd TCP → jcardsim bridge
    (real JavaCard applet execution, #403)

No boards, no physical cards. The job:

1. Installs pcscd + PCSC headers + JDK + Maven + pyscard.
2. Builds the Amperstrand-patched libifdvpcd from the pinned upstream
   (`tests/hardware/host/build_libifdvpcd_amperstrand.sh build`) — the
   stock driver's first absent-probe kills its event thread (#349
   class), the patch re-polls, which is what lets the bridge attach
   any time.
3. Installs a dedicated reader.conf entry (0x8C85, own .so copy —
   pcscd loads one instance per LIBPATH) and restarts pcscd.
4. Builds the bridge (`mvn -q package`), prepares the UNSIGNED
   jcardsim copy (the shipped jar is signed and rejects the shadow
   `javacard.security` classes).
5. Starts the echo-mode bridge and runs the suite (7 passed + 2
   mode-skips; the GP-contract tests skip when `gp` is absent).

SmartPGP mode (`--applet`) is NOT run here: the pinned CAP artifact
lives in bench storage (`.omo/gp-artifacts`, sha-pinned in
tests/hardware/nfc/J3R180-RUNBOOK.md) and is bench-gated.

Operational note (also in the test docstring): after a pcscd restart
the vpcd slot answers the ATR probe immediately but C-APDU transmits
can stay empty for ~2 minutes — the job waits the window out instead
of failing fast.
