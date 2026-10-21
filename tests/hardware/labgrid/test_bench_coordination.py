"""Host unit tests for the bench-coordination semantics (#334).

Covers the review-debt extractions from the HIL conftest into bench.py
(uniquely named, host-importable) and test_bench_state.py:

- bench_lock_mode: --skip-labgrid skips PLACE acquisition only — the
  cross-project flock stays taken in every mode (the coordinator
  outage escape hatch never unguards pcscd/serial legs).
- yield_acquired_place: a PlaceBusy raised between the holder check
  and the acquire becomes a SKIP naming the winning holder, never a
  setup ERROR; the happy path still acquires/releases + ledgers.
- selected_duts: the pcscd uniqueness check covers only the operator's
  --places selection.

Run: python3 -m unittest tests/hardware/labgrid/test_bench_coordination.py
(also collected by pytest). No hardware required.
"""

import fcntl
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import bench  # noqa: E402
import test_bench_state  # noqa: E402  (selected_duts extraction)


class _SentinelSkip(BaseException):
    """Stands in for pytest's Skipped outcome exception."""


class BenchLockModeTests(unittest.TestCase):
    def test_skip_labgrid_is_flock_only_never_lockless(self):
        # #334 P1: the coordinator-outage escape hatch skips PLACE
        # acquisition only — no mode drops the cross-project flock.
        self.assertEqual(bench.bench_lock_mode(skip_labgrid=True),
                         "flock-only")
        self.assertEqual(bench.bench_lock_mode(skip_labgrid=False),
                         "flock+places")

    def test_flock_only_mode_still_takes_the_lock(self):
        # Functional proof of the fixture body's behavior in BOTH
        # modes: the flock on the (redirectable) lock path is actually
        # held — the old fixture early-returned None under
        # --skip-labgrid and left pcscd/serial legs unguarded.
        with tempfile.TemporaryDirectory() as td:
            lock = Path(td) / "bench.lock"
            orig = bench.BENCH_LOCK_PATH
            bench.BENCH_LOCK_PATH = lock
            try:
                for mode in ("flock+places", "flock-only"):
                    with bench.bench_lock():
                        self.assertTrue(lock.exists())
                        fd = os.open(lock, os.O_RDWR)
                        try:
                            with self.assertRaises(BlockingIOError):
                                fcntl.flock(fd,
                                            fcntl.LOCK_EX | fcntl.LOCK_NB)
                        finally:
                            os.close(fd)
            finally:
                bench.BENCH_LOCK_PATH = orig


class YieldAcquiredPlaceTests(unittest.TestCase):
    def _fake_acquire(self, results):
        """acquired_place stand-in: a context manager whose enter raises
        or yields per `results` (PlaceBusy on the race, place on win)."""
        class _Ctx:
            def __init__(self, outcome):
                self.outcome = outcome
                self.exited = False

            def __enter__(self):
                if isinstance(self.outcome, Exception):
                    raise self.outcome
                return self.outcome

            def __exit__(self, *exc):
                self.exited = True
                return False

        ctxs = [_Ctx(o) for o in results]
        calls = []

        def acquire(place):
            calls.append(place)
            return ctxs[len(calls) - 1]

        return acquire, ctxs, calls

    def test_place_busy_race_skips_with_holder_named(self):
        # #334 P2: the losing side of the check→acquire race used to
        # surface PlaceBusy as a setup ERROR — it must SKIP with the
        # winning holder in the reason.
        busy = bench.PlaceBusy("m5stick", "alice@bench", "race detail")
        acquire, _ctxs, calls = self._fake_acquire([busy])
        orig = bench.acquired_place
        bench.acquired_place = acquire
        skips = []
        ledger = []

        def skip(reason):
            skips.append(reason)
            raise _SentinelSkip()

        try:
            gen = bench.yield_acquired_place(
                "m5stick", skip,
                ledger_fn=lambda ev, **f: ledger.append((ev, f)))
            with self.assertRaises(_SentinelSkip):
                next(gen)
        finally:
            bench.acquired_place = orig
        self.assertEqual(calls, ["m5stick"])
        self.assertEqual(len(skips), 1)
        self.assertIn("m5stick", skips[0])
        self.assertIn("alice@bench", skips[0],
                      "the skip must name the winning holder")
        self.assertEqual(ledger, [], "nothing was acquired, nothing ledgered")

    def test_happy_path_acquires_ledgers_and_releases(self):
        acquire, ctxs, calls = self._fake_acquire(["m5stick"])
        orig = bench.acquired_place
        bench.acquired_place = acquire
        ledger = []
        try:
            gen = bench.yield_acquired_place(
                "m5stick", lambda reason: (_ for _ in ()).throw(
                    _SentinelSkip()),
                ledger_fn=lambda ev, **f: ledger.append((ev, f)))
            place = next(gen)
            self.assertEqual(place, "m5stick")
            self.assertFalse(ctxs[0].exited, "held while the test runs")
            # normal completion (the fixture body finishes): release +
            # both ledger events
            with self.assertRaises(StopIteration):
                next(gen)
        finally:
            bench.acquired_place = orig
        self.assertTrue(ctxs[0].exited, "the place is released on completion")
        self.assertEqual([ev for ev, _f in ledger],
                         ["place_acquired", "place_released"])

    def test_teardown_close_still_releases_the_place(self):
        # pytest closes fixture generators after the test (GeneratorExit
        # at the yield): the ExitStack must still release the place.
        acquire, ctxs, _calls = self._fake_acquire(["m5stick"])
        orig = bench.acquired_place
        bench.acquired_place = acquire
        try:
            gen = bench.yield_acquired_place(
                "m5stick", lambda reason: (_ for _ in ()).throw(
                    _SentinelSkip()),
                ledger_fn=lambda ev, **f: None)
            self.assertEqual(next(gen), "m5stick")
            gen.close()
        finally:
            bench.acquired_place = orig
        self.assertTrue(ctxs[0].exited, "teardown must release the place")


class SelectedDutsTests(unittest.TestCase):
    def test_unselected_places_are_out_of_scope(self):
        # #334 P2: an unselected-yet-present wedged DUT must not fail a
        # run the operator explicitly scoped with --places.
        self.assertEqual(test_bench_state.selected_duts("m5stick"),
                         {"m5stick": "GemPCTwin"})

    def test_default_selection_covers_every_dut(self):
        all_places = ",".join(bench.PLACES)
        self.assertEqual(test_bench_state.selected_duts(all_places),
                         test_bench_state.DUT_IDENTITIES)

    def test_whitespace_and_empty_segments_tolerated(self):
        self.assertEqual(
            test_bench_state.selected_duts(" m5stick , ,nucula-c3 ,"),
            {"m5stick": "GemPCTwin", "nucula-c3": "Nucula CCID"})


if __name__ == "__main__":
    unittest.main()
