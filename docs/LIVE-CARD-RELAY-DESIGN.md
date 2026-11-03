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

## Prior-art reuse / gap table (condensed)

Full cited surveys: `docs/research/2026-11-03-prior-art-nfcgate-nero.md`
(NFCGate v2 + nero-nfc-reader-firmware, commit-pinned) and
`docs/research/2026-11-03-ntag424-ndef-pn7160-safety.md` (NTAG424 DNA
read-only safety + ndef-rs/nfcpy/PN7160 NCI references). Highlights:

| Source | Borrow for the relay | Borrow for conformance lab | License verdict |
|---|---|---|---|
| nfcgate v2 | 4-byte-BE-len+session framing is close to ours (2-byte VpcdBridge) — keep ours; their USENIX WOOT'20 latency data: relay fits card FWT only at FWT ≥ ~619 ms, BT-PAN beat WiFi for stability — budget our phone leg accordingly | capture/replay + pcapng export shape for future trace tooling | Apache-2.0 (app+server+proto, verified @ 35f73eef/4d32cc18) — GPLv3-compatible only → reference-only for our GPL-2.0+ firmware |
| nero v0.3.0 | reader-side ISO-DEP + their unit-test pattern (fake ISO-DEP peers); their CCID BWT time-extension heartbeat while the air interface waits is the mechanism our DUT wants against pcscd timeouts | NDEF T2/T4/T5 test vectors + their CCID dispatch test matrix (53 cases) + `spec-traceability.yaml` method (complements our greatspectations quote gate: theirs maps normative literals→symbols, ours verifies quotes verbatim) | Apache-2.0 (verified @ b0f5a486, SPDX headers) — GPLv3-compatible only → reference-only; test VECTORS and normative values are spec facts, reusable freely |
| Amperstrand/ntag424 + bolty-rs | read-only NTAG424 Type-4 APDU session (GET_VERSION/CC/NDEF reads; AUTH and anything key/SDM touching = forbidden) | safe-session test vectors | in-org (GPL family) |
| ndef-rs | — | NDEF parse coverage for T4 (URI records missing per survey) | GPL-3.0-or-later — reference |
| nfcpy | — | host-side ISO-DEP oracle for differential legs | EUPL — unlinked host tool only |
| NXP libnfc-nci (linux) | NCI RF activation/deactivation timing reference (already our reference-first source family) | same | Apache-2.0 |

Concrete gaps the survey found in OUR stack (drive future work):
per-hop timing instrumentation at every relay leg, a broker↔DUT
fake-ISO-DEP unit-test pattern, and an NDEF T4 parser conformance set
(nero's vectors) to test our readers' NDEF paths.
