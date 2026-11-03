# Live-card relay design — ISO-DEP APDU relay (kickoff 2026-11-03)

Target architecture (per kickoff brief), with each layer's current
status:

```
J3R180 or NTAG424 (physical)          [BLOCKED: fixtures not located]
       ⇅ ISO-DEP
Phone B: ReaderMode + IsoDep          [DESIGNED here; blocked on moto g]
       ⇅ adb forward / authenticated local channel
Host relay broker = hce_relay.py      [IMPLEMENTED: backends + session
                                       contract + journal; host-tested]
       ⇅ adb forward tcp:4871 (same framing, reverse direction)
Phone A (S20): HostApduService        [LIVE: link PASS, dispatch PASS]
       ⇅ ISO-DEP over 13.56 MHz
Nucula PN7160 / M5Stick / ACR1252     [nucula coupling PASS; pcscd
                                       presence blocked firmware-side]
       ⇅ CCID/PCSC
Host test runner                      [probes written; see evidence]
```

## Wire format (both phone links)

2-byte BIG-ENDIAN length + payload — the org.amperstrand.VpcdBridge
framing, identical to `RelayServer.java`. Frames from phone to host:
C-APDU bytes, or 1-byte controls `0x01` (ISO-DEP link deactivated),
`0x02` (hello). Host to phone: the exact R-APDU bytes.

The phone side never blocks the NFC main thread
(`processCommandApdu` returns null; a worker thread calls
`sendResponseApdu`), and with no host / host timeout it answers SW
6F00 — loud, never a silent fake card.

## Backend interface (`host-tools/hce_relay.py`)

```python
class CardBackend(ABC):
    def connect(self) -> None: ...      # card activation (lazy, first C-APDU)
    def disconnect(self) -> None: ...
    def exchange(self, capdu: bytes) -> bytes: ...  # raises BackendError
    def status(self) -> dict: ...
```

Implementations shipped: `StaticBackend` (fixed SW), `ScriptBackend`
(table + configurable miss SW, default 6A82), `PcscBackend` (real card
in a PC/SC reader — lazy pyscard import, reader chosen by unambiguous
name substring, single connection, NO retries).

### Session contract (the kickoff's critical design choice)

One phone socket ⇄ ONE backend card session, strictly sequential
exchanges. Lazy connect on first C-APDU; disconnect on link drop.
Backend failure = explicit error: no reply is sent for the failed
C-APDU, the phone socket closes, the process exits 1 — replies are
never re-paired across a failed exchange and nothing is retried
blindly. R-APDUs shorter than a status word are rejected before
framing. All locked by `tests/hardware/test_hce_relay.py` (21 tests).

### Evidence journal

`--journal FILE` writes JSONL: `session` header (backend status),
`meta` (`--meta k=v`, repeatable — commit, firmware hash, fixture
geometry), then `rx`/`tx` per exchange, `hello`/`deactivated`
controls, `error`/`drop`/`done` outcomes. Payload redaction is the
default: `len` + sha256-first-8 only; raw hex requires `--log-hex`
(sensitive card replies must not leak into artifacts by default).

## Phone B (ReaderMode) — design, blocked on hardware

Extends `host-tools/hce-relay-app` (no new GUI framework):

- `RelayReaderActivity`: transparent activity, `am start`ed over adb.
  `onCreate` → `NfcAdapter.enableReaderMode(this, callback,
  FLAG_READER_NFC_A | FLAG_READER_SKIP_NDEF_CHECK, extras)`; the
  activity finish()es on host session end.
- `onTagDiscovered(Tag)`: `IsoDep.get(tag)`, `connect()`, hold the
  handle; one worker thread; `close()` in `onPause`/host-EOF
  (lifecycle-aware teardown).
- New local-abstract socket `bench-reader-relay` (separate from the
  HCE socket; the two roles never share a socket). Wire: host→phone =
  C-APDU frame, phone→host = R-APDU frame + a `0x03 tag-present`
  control carrying tech list, `getMaxTransceiveLength`, timeouts.
- Session binding: one `Tag` per host session. Card removal
  (`IsoDep.isConnected == false` / IOException on transceive) →
  explicit `0x04 tag-lost` control, session invalidated — never a
  reconnect to a different card behind the same session, never a blind
  retransmit of a state-changing APDU.
- Bridging to the host: `adb forward tcp:4872
  localabstract:bench-reader-relay` — same authenticated-by-usb local
  channel as the HCE link; no network listener.

Status: UNSUPPORTED today (no Phone B hardware attached). When the
moto g lands, build via `host-tools/hce-relay-app/build.sh` (gradle-free
manual SDK build, stable bench key) and drive with the same brain:
`hce_relay.py --backend pcsc --reader Nucula` answers through the real
card... inverted for M3: the brain gains a `--reader-source` mode where
the phone is the BACKEND and the scripted table becomes the frontend.

## Security model

- Lab-only: adb-forward local channels, no unauthenticated network
  listener, no third-party terminals (kickoff hard constraint).
- Default-deny on real cards: `PcscBackend` transports bytes — the
  CALLER's allowlist discipline governs what is sent (AGENTS
  card-handling doctrine; SC-class commands never in loops).
- Proximity assumption documented, not enforced: the relay deliberately
  has no distance bounding — it is a bench instrument, not a payment
  path. Never point it at production systems.

## Limitations (state these in every verdict)

- Android HCE is AID-routed; UID/ATS are phone-controlled and never
  match a real card's — cross-HCE/real-card equality claims are
  UNSUPPORTED. Compare semantic APDU results and status words only.
- Type 2 memory/UID ops, ISO 15693, MIFARE Classic, raw RF bits, and
  precise S(WTX) generation are out of scope for this path.
- One active RF field per coupled pair; HCE activation adds
  100 ms-class latency (measured nucula APDU medians elsewhere: 78 ms).

## Fallback behavior

Backend error / card removal / host timeout / malformed frame → the
session dies with an explicit journal `error` record and nonzero exit;
the phone HCE answers 6F00 to the reader when hostless. Nothing
auto-reconnects mid-session by design.
