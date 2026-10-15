# SDIO clock speeds: specs, tradeoffs, and test results

This document summarizes manufacturer recommendations and tradeoffs for SD/SDXC clock speeds on the STM32F469 Discovery, and records results from the `sdio_speed_sweep` example.

**When to run the speed sweep**: The `sdio_speed_sweep` example is **optional**. Run it sparingly when you need to characterize your SD card (e.g. to choose a safe data-transfer frequency). Repeated runs can stress the card; for routine "SD card works" checks use `sdio_raw_test` instead.

## SD Physical Layer spec

- **Identification phase**: The host **must** use a clock between **0 and 400 kHz** during card identification (CMD0 through CMD3, etc.). The BSP always uses 400 kHz for this step.
- **Data transfer**: After the card leaves the identification state, the host may switch to a higher clock (up to 25 MHz for default speed, higher for high-speed modes). The BSP switches to the requested data-transfer frequency after a 500 ms stabilization delay (important for SDXC).

## STM32F4 SDIO (manufacturer specs)

- **Max SDIO_CK**: The SDIO peripheral has a **maximum ratio of 8:3 between SDIO_CK and PCLK2**. With typical PCLK2 (e.g. 90 MHz), that constrains SDIO_CK.
- **Practical maximum**: Community reports and ST examples often use **24–25 MHz** for SD cards. **48 MHz** is known to cause clock stability issues on some F4 setups (e.g. every 7th/8th cycle longer than expected).
- **HAL enum**: This BSP uses `stm32f4xx-hal`; available `ClockFreq` values include: 400 kHz (init only), 1, 4, 8, 12, 16, 24 MHz.

## Tradeoffs

| Goal            | Lower clock (e.g. 1–4 MHz)     | Higher clock (e.g. 12–24 MHz)   |
|-----------------|---------------------------------|----------------------------------|
| **Reliability** | Better on marginal cards/PCB   | Can trigger timeouts on some SDXC |
| **Throughput**  | Lower (e.g. ~1–4 MB/s range)   | Higher (e.g. up to ~12 MB/s)    |
| **Heat/power**  | Lower bus activity             | Higher activity                  |
| **Compatibility** | Works with most cards        | Some SDXC fail at 12+ MHz        |

**Recommendation**: Use **1 MHz** as the default for maximum reliability (validated on 64GB SDXC). Use the `sdio_speed_sweep` example to test 1, 4, 8, 12, 24 MHz on your card and fill in the results table below.

### Is 24 MHz “best”? Why stop at 24 MHz?

- **Best is card-specific.** For a card that passes all five frequencies, **24 MHz is the fastest we test** and gives the highest throughput. It is **not** “the most reliable” in general: lower clocks (1–4 MHz) are more reliable across different cards and marginal wiring; see the Tradeoffs table above.
- **STM / best practice:** ST examples and community typically use **24–25 MHz** for SD on F4. The HAL’s maximum data-transfer frequency we use is **24 MHz**. **48 MHz** is known to cause SDIO clock stability issues on many F4 setups (e.g. stretched cycles), so we do **not** test or recommend above 24 MHz unless the HAL and your hardware are known to support it.
- **Speed differences:** When you run the sweep **interactively** (probe-rs with RTT), the firmware logs **MB/s** per frequency (using the DWT cycle counter). The automated one-freq-per-run script does not capture that; it only records PASS/FAIL. So to see “X MB/s at 1 MHz vs Y MB/s at 24 MHz” in our testing, run probe-rs interactively and copy the defmt output. Typical ratio: 24 MHz is roughly **20–24×** faster than 1 MHz in throughput.
- **Why stop at 24 MHz?** The `stm32f4xx-hal` `ClockFreq` enum offers 1, 4, 8, 12, 16, 24 MHz for data transfer. We test up to **24 MHz** as the practical maximum recommended for this BSP; going higher would require a different HAL/clock setup and is not recommended without verifying SDIO clock quality.

### Is 24 MHz notably faster than 1 MHz? Did our testing show it?

- **Yes, 24 MHz is much faster in theory and in practice.** Clock speed scales roughly with throughput: 4-bit SDIO at 24 MHz can reach on the order of **~12 MB/s** (with protocol overhead); at 1 MHz you get on the order of **~0.5 MB/s**. So the ratio is roughly **20–24×**.
- **Our automated testing did not log throughput.** The one-freq-per-run sweep records only PASS/FAIL per frequency. So we have not recorded “X MB/s at 1 MHz vs Y MB/s at 24 MHz” in the automated logs. If you run probe-rs **interactively**, the sweep now logs **MB/s** per frequency (DWT cycle counter); you can paste that into `logs/OBSERVED-RESULTS.md` to document the speed difference.
- **What’s normal for devices?** Many devices **optimize for compatibility**: they use a conservative default (e.g. 1–4 MHz or “default speed” per SD spec) so all cards work, then optionally allow a higher speed after negotiation or user config. Others (e.g. some STM32 Cube examples) set **24–25 MHz** right after init and assume the card supports it. So there is a split: “safe default” vs “fast default.”
- **What do other projects do?** STM32 HAL/Cube often uses **24 MHz** for data transfer after the 400 kHz identification phase. Projects that prioritize reliability (e.g. industrial, multi-vendor cards) often use **lower** clocks (1–10 MHz) or make the clock configurable. So: **we are conservative** (1 MHz default); others often pick 24 MHz when they control the card type.
- **What should we do?** Keep **1 MHz as the BSP default** so new projects and unknown cards are safe. For **your** card, use the results table: if your sweep shows all pass up to 24 MHz, use `init_card_at_freq(..., ClockFreq::F24Mhz)` (or 8/12 MHz if you prefer a safety margin). We do not change the default to 24 MHz because that would risk timeouts on marginal cards; the table documents when it’s safe to raise speed.

### Reasoning for default (1 MHz)

- **Reliability first**: Many SD cards and SDXC cards work at 1 MHz across boards and wiring; higher speeds can cause timeouts or corruption on marginal hardware.
- **Spec and practice**: Identification is always 0–400 kHz; data transfer may be raised. We init at 400 kHz then switch to 1 MHz so the card is in a known-good state before any optional speed increase.
- **Using the results table**: After you run `sdio_speed_sweep` and record which of 1, 4, 8, 12, 24 MHz pass (see table below), you can choose a higher default for that specific card (e.g. 4 or 8 MHz) in application code via `init_card_at_freq(..., ClockFreq::F4Mhz)`. The BSP keeps **1 MHz** as the default in `init_card()` so new projects stay safe; raise speed only when the table shows the card supports it.

### How we tested (1, 4, 8, 12, 24 MHz)

The `sdio_speed_sweep` example runs a short read test (256 blocks) at each of 1, 4, 8, 12, and 24 MHz, prints PASS/FAIL per frequency, then a RECOMMENDATION (highest passing speed) and TRADEOFFS, then a full 10 MiB read at 1 MHz. **We have run this on the STM32F469I-DISCO** (one frequency per run via `./scripts/run-sweep-remote-capture.sh`); results are captured via a results buffer and written to `logs/sweep_analysis.txt`. To add a row for your card:

1. Run the sweep: `./scripts/run-sweep-remote-capture.sh` (builds per frequency, runs on remote host, retrieves and parses results), or run probe-rs interactively on the host and copy defmt output.
2. Add a row to the table below with date, card identifier, ✅/❌ per frequency, and Notes (e.g. RECOMMENDATION from the script).

## Test results (STM32F469I-DISCO)

**Automated run 2026-03-08:** One frequency per run via `run-sweep-remote-capture.sh`; all five frequencies (1, 4, 8, 12, 24 MHz) passed; RECOMMENDATION 24 MHz. Row added below.

| Date       | Card (brand/capacity) | 1 MHz | 4 MHz | 8 MHz | 12 MHz | 24 MHz | Notes |
|------------|------------------------|-------|-------|-------|--------|--------|-------|
| 2026-03-08 | (board run; card TBD)  | ✅    | ✅    | ✅    | ✅     | ✅     | One-freq-per-run sweep; RECOMMENDATION: 24 MHz |
| 2026-03-06 | (example 64GB SDXC)    | ✅    | —     | —     | ❌     | —      | 12 MHz: SoftwareTimeout |
| (add row)  | (your card)            |       |       |       |        |        | Run `./scripts/run-sweep-remote-capture.sh` or interactive probe-rs; see "How we tested" above |

---

## References

- SD Physical Layer Specification (e.g. v4.10+): identification clock 0–400 kHz; data transfer clock higher.
- ST AN5200: Getting started with STM32H7 SDMMC (similar concepts for F4).
- STM32F4 Reference Manual RM0386: SDIO section; PCLK2 vs SDIO_CK ratio.
- STM32F42x/43x Errata: SDIO clock/flow-control notes.
