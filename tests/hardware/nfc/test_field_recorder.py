"""Host unit tests for the ACR1252 field recorder (no hardware).

Run: python3 -m unittest tests/hardware/nfc/test_field_recorder.py
(also collected by pytest). Everything hardware-shaped is duck-typed:
FakeCard/FakeHardware stand in for the PcscCard/PcscHardware seam, so
the battery runs anywhere pytest runs — the CI python-collect gate
env has no pyscard, and so does any dev box.

Doctrine locks:
- reader pinning is EXACT name (never substring, never index);
- SC-class APDUs are refused at the library send boundary;
- module scope stays stdlib-only (ast-walk tripwire below — the
  python-collect gate rule as a unit test);
- every loop is bounded (poll deadline, --max-events cap).
"""

import ast
import json
import sys
import tempfile
import unittest
from datetime import datetime
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import field_recorder  # noqa: E402


# ---------------------------------------------------------------------------
# Duck-typed hardware (the FakeSerial pattern of test_gempc_t1.py)
# ---------------------------------------------------------------------------

class FakeCard:
    """Duck type of PcscCard: .atr / .transmit / .close."""

    def __init__(self, atr=b"\x3b\x85\x80\x01\x80\x73\xc8\x21\x10\x0e",
                 responses=None):
        self.atr = bytes(atr)
        self.sent: list[bytes] = []
        self.responses: list = list(responses or [])  # (resp,sw)|Exception
        self.closed = False

    def transmit(self, apdu: bytes):
        self.sent.append(bytes(apdu))
        if not self.responses:
            return b"", 0x9000
        item = self.responses.pop(0)
        if isinstance(item, Exception):
            raise item
        return item

    def close(self):
        self.closed = True


class FakeHardware:
    """Duck type of PcscHardware: list_readers / connect / present."""

    def __init__(self, readers, card=None, connect_errors=0,
                 present_script=(True,)):
        self.readers = list(readers)
        self.card = card
        self.connect_errors = connect_errors
        self.connect_calls = 0
        self.present_script = list(present_script)
        self.present_calls = 0

    def list_readers(self):
        return list(self.readers)

    def connect(self, reader):
        self.connect_calls += 1
        if self.connect_calls <= self.connect_errors:
            raise field_recorder.NoCardError(
                "NoCardException: fake — no card in the field")
        return self.card

    def present(self, reader):
        self.present_calls += 1
        idx = min(self.present_calls - 1, len(self.present_script) - 1)
        return self.present_script[idx]


class FakeClock:
    """monotonic + sleep pair; sleep advances the clock by its arg."""

    def __init__(self, start=1000.0):
        self.now = start
        self.sleeps: list[float] = []

    def monotonic(self):
        return self.now

    def sleep(self, secs):
        self.sleeps.append(secs)
        self.now += secs


def make_recorder(tmpdir, hw, reader=field_recorder.DEFAULT_READER,
                  max_events=1000, clock=None):
    clock = clock or FakeClock()
    writer = field_recorder.JsonlWriter(Path(tmpdir) / "evidence.jsonl",
                                        max_events=max_events)
    rec = field_recorder.FieldRecorder(hw, writer, reader,
                                       sleep_fn=clock.sleep,
                                       monotonic_fn=clock.monotonic)
    return rec, writer, clock


def read_events(path) -> list[dict]:
    path = Path(path)
    if not path.exists():   # zero-event stage writes nothing (lazy open)
        return []
    lines = path.read_text().splitlines()
    return [json.loads(line) for line in lines]


# ---------------------------------------------------------------------------

class ScClassGuardTests(unittest.TestCase):
    """The AGENTS.md safe/unsafe table, re-locked for this copy of the
    guard (mirrors test_gempc_t1's lock of the same doctrine)."""

    def test_sc_class_prefixes_recognized(self):
        for bad in ("80500000", "84820000", "00E00000"):
            self.assertTrue(
                field_recorder.is_sc_class_apdu(bytes.fromhex(bad)), bad)

    def test_gp_get_data_tag_in_p1p2_recognized(self):
        self.assertTrue(
            field_recorder.is_sc_class_apdu(bytes.fromhex("80CA00E000")))
        self.assertTrue(
            field_recorder.is_sc_class_apdu(bytes.fromhex("84CA00E000")))

    def test_logical_channel_clas_normalized(self):
        self.assertTrue(
            field_recorder.is_sc_class_apdu(bytes.fromhex("8150000000")))
        self.assertTrue(
            field_recorder.is_sc_class_apdu(bytes.fromhex("8582000000")))
        self.assertTrue(
            field_recorder.is_sc_class_apdu(bytes.fromhex("81CA00E000")))

    def test_safe_apdus_pass(self):
        for ok in ("00A40400", "00CA006E", "80CA9F7F", "00A40400023132",
                   "00CA004F00"):
            self.assertFalse(
                field_recorder.is_sc_class_apdu(bytes.fromhex(ok)), ok)

    def test_short_apdu_is_not_sc_class(self):
        self.assertFalse(field_recorder.is_sc_class_apdu(b"\x80"))


class ModuleScopeTests(unittest.TestCase):
    """The python-collect gate rule as a test: hardware imports only
    ever INSIDE functions — never at module scope of a collected file."""

    def test_no_hardware_import_at_module_scope(self):
        tree = ast.parse(Path(field_recorder.__file__).read_text())
        nested = set()

        def walk(node, inside_function):
            for child in ast.iter_child_nodes(node):
                child_is_fn = isinstance(
                    child, (ast.FunctionDef, ast.AsyncFunctionDef))
                if not inside_function and isinstance(
                        child, (ast.Import, ast.ImportFrom)):
                    mods = []
                    if isinstance(child, ast.Import):
                        mods = [a.name for a in child.names]
                    else:
                        mods = [child.module or ""]
                    for m in mods:
                        self.assertFalse(
                            m.split(".")[0] in ("smartcard", "pyscard"),
                            f"hardware import at module scope: {m}")
                if not child_is_fn and not inside_function:
                    nested.add(id(child))
                walk(child, inside_function or child_is_fn)

        walk(tree, False)

    def test_default_reader_is_the_bench_oracle_pin(self):
        # GREAT EXPECTATION: the bench oracle's exact pcscd name (the
        # ACR1252 PICC interface, .208 slot 02 — labgrid README gp pin,
        # perflib records slot 01 on other benches). If this changes,
        # re-verify against the bench pcscd listing, never by guess.
        self.assertEqual(
            field_recorder.DEFAULT_READER,
            "ACS ACR1252 Dual Reader [ACR1252 Dual Reader PICC] 02 00")


class PinReaderTests(unittest.TestCase):
    NAMES = [
        "ACS ACR1252 Dual Reader [ACR1252 Dual Reader PICC] 02 00",
        "ACS ACR1252 Dual Reader [ACR1252 Dual Reader SAM] 01 00",
        "Cherry GmbH SmartTerminal ST-2xxx (ST2XXX-001) 02 00",
    ]

    def test_exact_match_resolves(self):
        self.assertEqual(field_recorder.pin_reader(
            self.NAMES, self.NAMES[0]), self.NAMES[0])

    def test_zero_matches_is_loud_with_listing(self):
        with self.assertRaises(field_recorder.RecorderError) as ctx:
            field_recorder.pin_reader(self.NAMES, "Some Other Reader 00 00")
        for name in self.NAMES:
            self.assertIn(name, str(ctx.exception))

    def test_substring_is_not_a_pin(self):
        with self.assertRaises(field_recorder.RecorderError):
            field_recorder.pin_reader(self.NAMES, "ACR1252")

    def test_multiple_exact_matches_is_loud(self):
        dup = [self.NAMES[0], self.NAMES[0]]
        with self.assertRaises(field_recorder.RecorderError):
            field_recorder.pin_reader(dup, self.NAMES[0])

    def test_empty_listing_names_itself(self):
        with self.assertRaises(field_recorder.RecorderError) as ctx:
            field_recorder.pin_reader([], "whatever")
        self.assertIn("no readers", str(ctx.exception))


class JsonlWriterTests(unittest.TestCase):
    def test_events_are_one_json_line_each(self):
        with tempfile.TemporaryDirectory() as tmp:
            w = field_recorder.JsonlWriter(Path(tmp) / "ev.jsonl")
            w.write("reader_list", None, {"readers": ["a", "b"]})
            w.write("card_atr", "a", {"atr": "3b85"})
            w.close()
            events = read_events(Path(tmp) / "ev.jsonl")
            self.assertEqual([e["kind"] for e in events],
                             ["reader_list", "card_atr"])
            self.assertEqual(events[0]["reader"], None)
            self.assertEqual(events[1]["reader"], "a")
            self.assertEqual(events[0]["data"], {"readers": ["a", "b"]})

    def test_ts_is_tz_aware_iso8601(self):
        with tempfile.TemporaryDirectory() as tmp:
            w = field_recorder.JsonlWriter(Path(tmp) / "ev.jsonl")
            w.write("reader_list", None, {})
            w.close()
            ts = read_events(Path(tmp) / "ev.jsonl")[0]["ts"]
            parsed = datetime.fromisoformat(ts)
            self.assertIsNotNone(parsed.tzinfo)

    def test_file_is_append_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "ev.jsonl"
            w1 = field_recorder.JsonlWriter(path)
            w1.write("card_atr", "r", {"atr": "00"})
            w1.close()
            w2 = field_recorder.JsonlWriter(path)
            w2.write("poll_presence", "r", {"event": "insert"})
            w2.close()
            self.assertEqual(len(read_events(path)), 2)

    def test_cap_raises_and_stop_marker_bypasses(self):
        with tempfile.TemporaryDirectory() as tmp:
            w = field_recorder.JsonlWriter(Path(tmp) / "ev.jsonl",
                                           max_events=1)
            w.write("card_atr", "r", {"atr": "00"})
            with self.assertRaises(field_recorder.EventCapReached):
                w.write("card_atr", "r", {"atr": "11"})
            w.write_final("recorder_stop", "r", {"reason": "max-events"})
            w.close()
            events = read_events(Path(tmp) / "ev.jsonl")
            self.assertEqual([e["kind"] for e in events],
                             ["card_atr", "recorder_stop"])

    def test_per_kind_counts(self):
        with tempfile.TemporaryDirectory() as tmp:
            w = field_recorder.JsonlWriter(Path(tmp) / "ev.jsonl")
            w.write("card_atr", "r", {})
            w.write("apdu_exchange", "r", {})
            w.write("apdu_exchange", "r", {})
            w.close()
            self.assertEqual(w.count, 3)
            self.assertEqual(w.counts,
                             {"card_atr": 1, "apdu_exchange": 2})

    def test_invalid_cap_rejected(self):
        with self.assertRaises(ValueError):
            field_recorder.JsonlWriter("/nonexistent/ev.jsonl",
                                       max_events=0)


class RecorderStageTests(unittest.TestCase):
    def test_reader_list_event(self):
        with tempfile.TemporaryDirectory() as tmp:
            hw = FakeHardware(["reader A 00 00", "reader B 01 00"])
            rec, writer, _clock = make_recorder(tmp, hw)
            names = rec.record_reader_list()
            self.assertEqual(names, ["reader A 00 00", "reader B 01 00"])
            events = read_events(writer.path)
            self.assertEqual(events[0]["kind"], "reader_list")
            self.assertEqual(events[0]["reader"], None)
            self.assertEqual(events[0]["data"]["readers"], names)

    def test_capture_atr_success(self):
        with tempfile.TemporaryDirectory() as tmp:
            card = FakeCard()
            rec, writer, clock = make_recorder(tmp, FakeHardware(
                [field_recorder.DEFAULT_READER], card=card))
            got = rec.capture_atr()
            self.assertIs(got, card)
            events = read_events(writer.path)
            self.assertEqual(events[0]["kind"], "card_atr")
            self.assertEqual(events[0]["data"]["atr"], card.atr.hex())
            self.assertEqual(events[0]["data"]["atr_len"], len(card.atr))
            self.assertEqual(clock.sleeps, [])  # no retries needed

    def test_capture_atr_retries_then_succeeds(self):
        with tempfile.TemporaryDirectory() as tmp:
            card = FakeCard()
            rec, writer, clock = make_recorder(tmp, FakeHardware(
                [field_recorder.DEFAULT_READER], card=card,
                connect_errors=2))
            got = rec.capture_atr(attempts=3, delay=0.25)
            self.assertIs(got, card)
            self.assertEqual(clock.sleeps, [0.25, 0.25])
            events = read_events(writer.path)
            self.assertEqual([e["kind"] for e in events],
                             ["card_atr"])  # only the success event

    def test_capture_atr_exhausts_bounded(self):
        with tempfile.TemporaryDirectory() as tmp:
            rec, writer, clock = make_recorder(tmp, FakeHardware(
                [field_recorder.DEFAULT_READER], connect_errors=99))
            got = rec.capture_atr(attempts=3, delay=0.5)
            self.assertIsNone(got)
            self.assertEqual(clock.sleeps, [0.5, 0.5])  # bounded retries
            events = read_events(writer.path)
            self.assertEqual(events[0]["data"]["error"],
                             "NoCardException: fake — no card in the field")

    def test_exchange_apdus_records_request_response_sw_ms(self):
        with tempfile.TemporaryDirectory() as tmp:
            card = FakeCard(responses=[(b"\x6a\x82", 0x6A82),
                                       (b"", 0x9000)])
            rec, writer, _clock = make_recorder(tmp, FakeHardware(
                [field_recorder.DEFAULT_READER], card=card))
            apdus = [bytes.fromhex("00A40400023132"),
                     bytes.fromhex("00CA006E00")]
            rec.exchange_apdus(card, apdus)
            self.assertEqual(card.sent, apdus)
            events = read_events(writer.path)
            self.assertEqual([e["kind"] for e in events],
                             ["apdu_exchange", "apdu_exchange"])
            first, second = events
            self.assertEqual(first["data"]["request"], "00a40400023132")
            self.assertEqual(first["data"]["response"], "6a82")
            self.assertEqual(first["data"]["sw"], "6A82")
            self.assertGreaterEqual(first["data"]["ms"], 0.0)
            self.assertEqual(second["data"]["sw"], "9000")
            self.assertEqual(second["data"]["response"], "")

    def test_exchange_apdus_refuses_sc_class_at_library_boundary(self):
        # the gempc_t1 Codex-P1 lesson: the guard must fire in the
        # library, not only the CLI — nothing may loop SC-class APDUs
        # through this API
        with tempfile.TemporaryDirectory() as tmp:
            card = FakeCard()
            rec, _writer, _clock = make_recorder(tmp, FakeHardware(
                [field_recorder.DEFAULT_READER], card=card))
            for bad in ("8050000000", "8482000000", "80CA00E000",
                        "8150000000"):
                with self.assertRaises(field_recorder.RecorderError,
                                       msg=bad):
                    rec.exchange_apdus(card, [bytes.fromhex(bad)])
            self.assertEqual(card.sent, [])  # nothing reached the card

    def test_exchange_apdus_records_transport_error_and_continues(self):
        with tempfile.TemporaryDirectory() as tmp:
            card = FakeCard(responses=[RuntimeError("reader went away"),
                                       (b"", 0x9000)])
            rec, writer, _clock = make_recorder(tmp, FakeHardware(
                [field_recorder.DEFAULT_READER], card=card))
            rec.exchange_apdus(card, [bytes.fromhex("00CA006E00"),
                                      bytes.fromhex("00CA004F00")])
            events = read_events(writer.path)
            self.assertIn("error", events[0]["data"])
            self.assertIn("reader went away", events[0]["data"]["error"])
            self.assertEqual(events[1]["data"]["sw"], "9000")

    def test_poll_presence_records_initial_then_transitions(self):
        with tempfile.TemporaryDirectory() as tmp:
            clock = FakeClock()
            # 4 samples fit in duration 1.0 (t=0, .25, .5, .75):
            # T T F T -> initial, remove, insert
            rec, writer, _ = make_recorder(
                tmp, FakeHardware([field_recorder.DEFAULT_READER],
                                  present_script=[True, True, False,
                                                  True]),
                clock=clock)
            rec.poll_presence(interval=0.25, duration=1.0)
            events = read_events(writer.path)
            self.assertEqual([e["data"]["event"] for e in events],
                             ["initial", "remove", "insert"])
            self.assertEqual([e["data"]["present"] for e in events],
                             [True, False, True])
            self.assertEqual(events[-1]["data"]["elapsed_s"], 0.75)
            self.assertTrue(all(e["reader"] == field_recorder.DEFAULT_READER
                                for e in events))

    def test_poll_presence_is_bounded_by_deadline(self):
        with tempfile.TemporaryDirectory() as tmp:
            clock = FakeClock()
            hw = FakeHardware([field_recorder.DEFAULT_READER],
                              present_script=[True])
            rec, _writer, _ = make_recorder(tmp, hw, clock=clock)
            rec.poll_presence(interval=0.25, duration=1.0)
            # 4 samples fit before the deadline (t=0, .25, .5, .75) —
            # the last sleep runs the clock exactly to the deadline
            self.assertEqual(hw.present_calls, 4)
            self.assertEqual(clock.sleeps, [0.25, 0.25, 0.25, 0.25])

    def test_poll_presence_zero_duration_records_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            rec, writer, _ = make_recorder(tmp, FakeHardware([]))
            rec.poll_presence(interval=0.5, duration=0.0)
            self.assertEqual(read_events(writer.path), [])

    def test_poll_presence_rejects_nonpositive_interval(self):
        with tempfile.TemporaryDirectory() as tmp:
            rec, _writer, _ = make_recorder(tmp, FakeHardware([]))
            with self.assertRaises(ValueError):
                rec.poll_presence(interval=0.0, duration=1.0)

    def test_poll_presence_stops_at_event_cap(self):
        with tempfile.TemporaryDirectory() as tmp:
            clock = FakeClock()
            hw = FakeHardware([field_recorder.DEFAULT_READER],
                              present_script=[True, False])
            rec, writer, _ = make_recorder(tmp, hw, max_events=1,
                                           clock=clock)
            with self.assertRaises(field_recorder.EventCapReached):
                rec.poll_presence(interval=0.25, duration=100.0)
            # initial event filled the cap; the remove transition hit it
            self.assertEqual(hw.present_calls, 2)
            events = read_events(writer.path)
            self.assertEqual([e["kind"] for e in events], ["poll_presence"])


class RunSessionTests(unittest.TestCase):
    def _run(self, tmp, hw, apdus=(), interval=0.25, duration=0.5,
             max_events=1000):
        rec, writer, clock = make_recorder(tmp, hw, max_events=max_events)
        reason = rec.run(apdus=list(apdus), interval=interval,
                         duration=duration)
        return reason, read_events(writer.path), writer, clock

    def test_full_session_event_order_and_counts(self):
        card = FakeCard(responses=[(b"", 0x9000), (b"\x6f", 0x9000)])
        hw = FakeHardware([field_recorder.DEFAULT_READER], card=card,
                          present_script=[False, True, True])
        with tempfile.TemporaryDirectory() as tmp:
            reason, events, writer, _clock = self._run(
                tmp, hw,
                apdus=[bytes.fromhex("00A40400023132"),
                       bytes.fromhex("00CA006E00")])
            self.assertEqual(reason, "completed")
            kinds = [e["kind"] for e in events]
            self.assertEqual(kinds[0], "recorder_meta")
            self.assertEqual(kinds[1], "reader_list")
            self.assertEqual(kinds[2], "card_atr")
            self.assertEqual(kinds[3:5],
                             ["apdu_exchange", "apdu_exchange"])
            self.assertEqual(kinds[5:-1],
                             ["poll_presence", "poll_presence"])
            self.assertEqual(kinds[-1], "recorder_stop")
            stop = events[-1]["data"]
            self.assertEqual(stop["reason"], "completed")
            # self-consistent marker: events == file lines, counts sum
            # to events (marker included)
            self.assertEqual(stop["events"], len(events))
            self.assertEqual(sum(stop["counts"].values()), len(events))
            self.assertEqual(stop["counts"]["recorder_stop"], 1)
            self.assertEqual(stop["counts"]["apdu_exchange"], 2)
            self.assertTrue(card.closed)
            meta = events[0]["data"]
            self.assertEqual(meta["apdus"],
                             ["00a40400023132", "00ca006e00"])

    def test_session_without_card_still_polls(self):
        hw = FakeHardware([field_recorder.DEFAULT_READER],
                          connect_errors=99, present_script=[False])
        with tempfile.TemporaryDirectory() as tmp:
            reason, events, _writer, _clock = self._run(tmp, hw)
            self.assertEqual(reason, "completed")
            kinds = [e["kind"] for e in events]
            self.assertEqual(kinds,
                             ["recorder_meta", "reader_list", "card_atr",
                              "poll_presence", "recorder_stop"])
            self.assertIn("error", events[2]["data"])

    def test_unpinned_reader_raises_from_run(self):
        hw = FakeHardware(["Some Other Reader 00 00"])
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(field_recorder.RecorderError):
                self._run(tmp, hw)
            # the loud pin error carries the full listing
            # (validated in PinReaderTests); the stop-marker-on-error
            # path is main()'s job, exercised via the writer tests

    def test_max_events_cap_ends_cleanly(self):
        card = FakeCard()
        hw = FakeHardware([field_recorder.DEFAULT_READER], card=card)
        with tempfile.TemporaryDirectory() as tmp:
            reason, events, writer, _clock = self._run(
                tmp, hw, duration=100.0, max_events=2)
            self.assertEqual(reason, "max-events")
            self.assertEqual(events[-1]["kind"], "recorder_stop")
            self.assertEqual(events[-1]["data"]["reason"], "max-events")
            # cap honored: 2 capped events + the bypass stop marker
            self.assertEqual(writer.count, 3)


class CliTests(unittest.TestCase):
    def _parse_err(self, argv):
        with self.assertRaises(SystemExit) as ctx:
            field_recorder.build_parser().parse_args(argv)
        self.assertEqual(ctx.exception.code, 2)

    def test_out_is_required(self):
        self._parse_err([])

    def test_sc_class_apdu_refused_at_parse(self):
        for bad in ("80500000", "8482000000", "80CA00E000", "00E00000"):
            self._parse_err(["--out", "x.jsonl", "--apdu", bad])

    def test_non_hex_apdu_refused(self):
        self._parse_err(["--out", "x.jsonl", "--apdu", "zzzz"])

    def test_short_apdu_refused(self):
        self._parse_err(["--out", "x.jsonl", "--apdu", "00A4"])

    def test_safe_apdus_parse(self):
        args = field_recorder.build_parser().parse_args([
            "--out", "x.jsonl", "--apdu", "00A40400023132",
            "--apdu", "00CA006E00"])
        self.assertEqual([a.hex() for a in args.apdu],
                         ["00a40400023132", "00ca006e00"])

    def test_defaults(self):
        args = field_recorder.build_parser().parse_args(["--out", "x"])
        self.assertEqual(args.reader, field_recorder.DEFAULT_READER)
        self.assertEqual(args.interval, 0.5)
        self.assertEqual(args.duration, 10.0)
        self.assertEqual(args.max_events, 1000)


if __name__ == "__main__":
    unittest.main()
