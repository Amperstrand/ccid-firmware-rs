# Fuzz targets (cargo-fuzz)

Coverage-guided fuzzing of the host-buildable workspace crates. A crash
found here is a **finding** — record it, reproduce it, never silence it.

| Target | Fuzzed surface | Invariants asserted |
| --- | --- | --- |
| `fuzz_atr_parse` | `ccid_protocol::atr::parse_atr` + `verify_atr_tck` with arbitrary byte slices (generalizes the random-input tests in `crates/ccid-protocol/src/atr.rs`) | no panic; inputs `< 2` bytes return default params; T=1 TCK verdict always equals its XOR definition |
| `fuzz_serial_frame_parser` | `ccid_transport_serial::FrameParser::feed` with an arbitrary byte stream (bounded to 4096 bytes per input) | no panic; an accepted `Command` frame is always structurally well-formed with a valid LRC — `SYNC 0x03`, `CTRL_ACK 0x06`, CCID message, then the XOR of everything before it |

## Prerequisites

- nightly toolchain with `rust-src` (cargo-fuzz builds with `-Zbuild-std`)
- cargo-fuzz 0.13: `cargo +nightly install cargo-fuzz`
- network access on first build (fetches `libfuzzer-sys`)

## Build

From this directory (or from the repo root — cargo-fuzz detects either):

```sh
cd fuzz
cargo +nightly fuzz build
```

## Run

```sh
# Timeboxed run (60s smoke)
cargo +nightly fuzz run fuzz_atr_parse -- -max_total_time=60
cargo +nightly fuzz run fuzz_serial_frame_parser -- -max_total_time=60

# Longer overnight run via the Track C hook (persistent corpus + artifact
# collection under bolty-rs/tools/hil/overnight/results/fuzz/)
python3 ../../bolty-rs/tools/hil/overnight/track_c_fuzz.py \
    --target fuzz_atr_parse --seconds 1200
```

Corpus and artifacts land in `fuzz/corpus/` and `fuzz/artifacts/` (both
gitignored). The overnight hook keeps its own persistent per-target corpus
under `results/fuzz/corpus/<target>/` so runs build on each other.

## Reproducing a crash

libFuzzer saves the crashing input as `artifacts/crash-<sha1>` (likewise
`timeout-`, `oom-`, `leak-` prefixed artifacts for other finding types).
Replay it directly by passing the artifact file as the corpus argument —
libFuzzer runs the single input and exits:

```sh
cargo +nightly fuzz run fuzz_atr_parse artifacts/crash-<sha1>
cargo +nightly fuzz run fuzz_serial_frame_parser artifacts/crash-<sha1>
```

For ASAN debug output add `-O` and `RUST_BACKTRACE=1`. Report the finding
in the overnight results (the runner hook copies the artifact and writes a
`crash` row automatically); do not delete or weaken the target.
