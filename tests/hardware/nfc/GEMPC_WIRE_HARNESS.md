# Raw GemPC wire harness vs the pcscd path

`tests/hardware/nfc/` hosts two ways to talk to the serial CCID readers
(m5stick FTDI, nucula CDC). They answer different questions — pick the
one that matches the failure you are chasing.

| | pcscd path | raw wire harness |
|---|---|---|
| Tools | `conformance_battery.py` (via pcscd), `test_differential.py`, `test_gpg.py` | `gempc_t1.py` (this harness), `pcscd_strace_decode.py` |
| Who owns the port | pcscd (reader.conf entry enabled) | the harness — **pcscd must be stopped under the bench flock** |
| What it proves | end-user reality: libccidtwin + pcsc-lift T=1 + our endpoint, as shipped | deterministic protocol control: exact frames, exact T=1 blocks, LRC/EDC verification |
| Use when | regressing interop, soak/restart resilience, card-level differential vs the ACR1252 oracle | arming the T=1 endpoint by hand, injecting chained I-blocks / WTX dances / retransmit storms, diffing our wire against what libccidtwin actually sends |
| Serial params | pcscd's (libccidtwin hardcodes 115200) | safe-open, 115200 **8N2** (`--stopbits 1` for the nucula CDC — 8N1 is framing death on the FTDI, not even echo) |

## gempc_t1.py — session driver + CLI

    python3 tests/hardware/nfc/gempc_t1.py --probe            # m5stick by default
    python3 tests/hardware/nfc/gempc_t1.py --probe --stopbits 1   # nucula CDC
    python3 tests/hardware/nfc/gempc_t1.py --probe --apdu 00CA006E

`--probe` runs the exact libccidtwin opening order and prints every
parsed frame and T=1 block:

1. `GetSlotStatus`
2. `IccPowerOn` (prints the ATR)
3. `SetParameters(T=1)` — **bProtocolNum=1 rides the CCID header-tail
   byte index 3**. This is the byte that ARMS the firmware's T=1
   endpoint: a bare-payload frame leaves it 0 = T=0, and every XfrBlock
   then goes to the card as a raw TPDU (answered 6700/6E00). This was
   the 2026-10-20 session's root cause.
4. `XfrBlock` carrying `S(IFS request)` `[00 C1 01 FE]+LRC` — the EDC
   is load-bearing: without it the endpoint correctly answers
   `R(retransmit)` and `send_ifsc` stays 32.
5. `SELECT` by AID (pure read; card-safety doctrine — SC-class APDUs
   are refused).

The library surface is importable (`T1Session`, `make_frame`,
`parse_frames`, `split_echo`, `i_block`/`r_block`/`s_block` builders,
`parse_t1` with EDC verification) and stdlib-only at module scope —
pyserial is imported lazily inside the hardware functions (CI
python-collect gate). Host-side behavior is unit-locked in
`test_gempc_t1.py`, including the WTX dance against a scripted port.

### Wire facts encoded (bench-proven 2026-10-20)

- Frame = `SYNC 0x03 | CTRL 0x06 | 10-byte CCID header (type,
  dwLength LE, slot, seq, bBWI/bProtocolNum, 2x RFU) | payload |
  XOR-LRC`; payload starts at byte 12.
- The device **echoes** every frame before replying (`split_echo`).
- The firmware is bilingual: bare-APDU XfrBlock payloads get bare
  DataBlock responses; T=1-block payloads get T=1-wrapped responses.
- T=1 answers may defer behind `S(WTX request)` `[00 C3 01 m]+LRC` —
  answer `S(WTX response)` `[00 E3 01 m]+LRC` in a further XfrBlock
  and the I-block follows (`T1Session.xfr_block` runs this dance).
- T=1 PCB layout mirrors `firmware/esp32-ccid/src/t1.rs`
  (`parse_pcb`, ISO 7816-3 §11.4.2 as implemented by pcsc-lite
  `proto-t1.c`): I `0x00|ns<<6|more<<5|nr<<4`, R `0x80|nr<<4|err`,
  S `0xC0|kind` with responses setting `0x20` (IFS `C1→E1`, WTX
  `C3→E3`).

### Safety rules (non-negotiable)

- pcscd holds the port — the CLI stops it under the bench flock via
  `esp32_flash.pcscd_lock_acquire()` (released/restarted in `finally`).
  `--no-pcscd-lock` exists only for "pcscd is already stopped".
- DTR/RTS low BEFORE open (`gempc_smoke.open_safe`) — asserted-DTR
  opens toggle the FTDI auto-reset line.
- Card-safety doctrine (AGENTS.md): SELECT/CPLC-class APDUs only;
  never SC-class commands (INIT_UPDATE …) — the harness refuses them.

## pcscd_strace_decode.py — what did libccidtwin actually send?

    strace -f -e trace=read,write -xx -s 4096 -o TRACE.txt -- pcscd -f
    python3 tests/hardware/nfc/pcscd_strace_decode.py TRACE.txt [--device Hades]

Pins the GemPC-talking fd **by device** (resolves `/proc/<pid>/fd/<n>`
symlinks; `--fd` when pcscd is already dead), reassembles the read
stream in capture order, and decodes every GemPC frame + T=1 block with
LRC/EDC verification — the TX side is exactly the byte sequence
`gempc_t1.T1Session` reproduces live.

> **⚠ ALWAYS `-xx`.** Without it strace emits octal (`\012`) and letter
> (`\f`, `\n`, `\t`) escapes — octal/letter escapes silently corrupt
> `\f`/`\n`/`\t` bytes in any decoder that assumes `\xNN`. The decoder
> understands the legacy forms and warns loudly when it meets them;
> treat the warning as "re-capture", not "trust the decode".

## Provenance

Promoted from the 2026-10-20 bench session scratch (`t1_session.py`,
`t1_verify.py`, `replay_libccid.py`); every byte-level claim above was
proven against the m5stick GemPC wire that day. Hardware verification
of this harness itself: pending bench run (parent task).
