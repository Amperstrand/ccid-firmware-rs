//! boards-gen — the boards-as-data generator (design §2.1).
//!
//! Generates `crates/boards/src/nucula_gen.rs` from a PINNED revision of
//! `zeugmaster/nucula-board`'s `docs/netlist.xml`, guided by the sidecar
//! net-map `crates/boards/gen/nucula-nets.toml` (net renames upstream
//! change ONE sidecar line, not this parser).
//!
//! Explicit-command by design (§1.3): generated *const Rust*, checked in
//! with a provenance header; NEVER run from any build.rs (reproducible
//! builds, offline CI). Regenerate:
//!
//! ```text
//! cargo run -p boards-gen -- --board nucula
//! ```
//!
//! Facts the netlist cannot provide (the I2C address is an R23/R24 strap;
//! bus speeds, console, OTA family are bench/vendor facts) come from the
//! sidecar + this file's table template, and the generated table's host
//! test asserts equality with `pn7160-nci::board::NUCULA` — the bench
//! wins, a disagreement is a finding against the netlist revision.
//!
//! `--check` regenerates in memory and diffs against the checked-in file
//! (exit 1 on drift) — the `tools/union_matrix.py check` boards leg can
//! call it in CI so hand edits cannot survive.

use std::process::Command;

// ---------------------------------------------------------------------------
// sidecar (typed view of gen/nucula-nets.toml)
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct Sidecar {
    source: Source,
    anchors: Anchors,
    nfc: Nfc,
}

#[derive(Debug, serde::Deserialize)]
struct Source {
    repo: String,
    rev: String,
    netlist_path: String,
    netlist_sha256: String,
}

#[derive(Debug, serde::Deserialize)]
struct Anchors {
    mcu_ref: String,
    nfc_ref: String,
}

#[derive(Debug, serde::Deserialize)]
struct Nfc {
    i2c_addr: u8,
    nets: NfcNets,
}

#[derive(Debug, serde::Deserialize)]
struct NfcNets {
    sda: NetAnchor,
    scl: NetAnchor,
    irq: NetAnchor,
    ven: NetAnchor,
}

#[derive(Debug, serde::Deserialize)]
struct NetAnchor {
    net: String,
    pin: String,
}

// ---------------------------------------------------------------------------
// KiCad netlist scanning (hand-rolled, fixture-tested — the netlist is a
// flat <net><node/></net> shape; no XML dep by design: generator stays
// std + toml only)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Node {
    ref_: String,
    pinfunction: String,
}

#[derive(Debug, Clone)]
struct Net {
    name: String,
    nodes: Vec<Node>,
}

/// Extract `attr="value"` from a tag body (first occurrence).
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn parse_nets(text: &str) -> Vec<Net> {
    let mut nets = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("<net ") {
        let after = &rest[open..];
        let Some(close) = after.find("</net>") else { break };
        let block = &after[..close];
        // Tag header runs to the first '>' (the net element carries its
        // attributes inline).
        let header_end = block.find('>').unwrap_or(0);
        let header = &block[..header_end];
        let mut nodes = Vec::new();
        let mut node_rest = block;
        while let Some(n_open) = node_rest.find("<node ") {
            let n_after = &node_rest[n_open..];
            let Some(n_end) = n_after.find("/>") else { break };
            let n_tag = &n_after[..n_end];
            nodes.push(Node {
                ref_: attr(n_tag, "ref").unwrap_or_default().to_string(),
                pinfunction: attr(n_tag, "pinfunction").unwrap_or_default().to_string(),
            });
            node_rest = &n_after[n_end..];
        }
        nets.push(Net {
            name: attr(header, "name").unwrap_or_default().to_string(),
            nodes,
        });
        rest = &after[close + "</net>".len()..];
    }
    nets
}

/// The GPIO number of `mcu_ref` on `net`, with the NFC-side anchor
/// (`nfc_ref` pinfunction) proving we are on the right wire.
fn anchor_gpio(net: &Net, nfc_ref: &str, mcu_ref: &str, anchor_pin: &str) -> Result<u8, String> {
    let anchors: Vec<_> = net
        .nodes
        .iter()
        .filter(|n| {
            n.ref_ == nfc_ref
                && (n.pinfunction == anchor_pin
                    || n.pinfunction.starts_with(&format!("{anchor_pin}_")))
        })
        .collect();
    match anchors.len() {
        1 => {}
        0 => {
            return Err(format!(
                "net {}: no {} pinfunction {}",
                net.name, nfc_ref, anchor_pin
            ))
        }
        n => return Err(format!("net {}: {} anchor matches", net.name, n)),
    }
    let mcu: Vec<_> = net.nodes.iter().filter(|n| n.ref_ == mcu_ref).collect();
    let mcu = match mcu.as_slice() {
        [one] => one,
        [] => return Err(format!("net {}: no {} node", net.name, mcu_ref)),
        many => {
            return Err(format!(
                "net {}: {} nodes on one net ({})",
                net.name,
                mcu_ref,
                many.len()
            ))
        }
    };
    // ESP32-C3-MINI-1 symbol pin names: IO<gpio>_<module pin>, e.g. IO4_18.
    let io = mcu
        .pinfunction
        .strip_prefix("IO")
        .and_then(|rest| rest.split('_').next())
        .and_then(|num| num.parse::<u8>().ok())
        .ok_or_else(|| {
            format!(
                "net {}: {} pinfunction {} is not IO<gpio>_<pin>",
                net.name, mcu_ref, mcu.pinfunction
            )
        })?;
    Ok(io)
}

// ---------------------------------------------------------------------------
// netlist acquisition
// ---------------------------------------------------------------------------

fn sha256_of(bytes: &[u8]) -> Result<String, String> {
    let tmp = std::env::temp_dir().join(format!("boards-gen-netlist-{}.xml", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    let out = Command::new("sha256sum")
        .arg(&tmp)
        .output()
        .map_err(|e| format!("sha256sum (coreutils) not available: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    if !out.status.success() {
        return Err("sha256sum failed".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase())
}

fn fetch_netlist(sidecar: &Sidecar, local_path: Option<&str>) -> Result<Vec<u8>, String> {
    let bytes = match local_path {
        Some(p) => std::fs::read(p).map_err(|e| format!("read {p}: {e}"))?,
        None => {
            let uri = format!(
                "repos/{}/contents/{}?ref={}",
                sidecar.source.repo, sidecar.source.netlist_path, sidecar.source.rev
            );
            let out = Command::new("gh")
                .args(["api", &uri, "-H", "Accept: application/vnd.github.raw"])
                .output()
                .map_err(|e| format!("gh not available: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "gh api {uri} failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
            out.stdout
        }
    };
    let digest = sha256_of(&bytes)?;
    if digest != sidecar.source.netlist_sha256.to_lowercase() {
        return Err(format!(
            "netlist sha256 mismatch: sidecar pins {}, got {} — update gen/nucula-nets.toml (and review the diff)",
            sidecar.source.netlist_sha256, digest
        ));
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// code emission
// ---------------------------------------------------------------------------

fn emit(sidecar: &Sidecar, gpio: &NfcNets, gpio_pins: [u8; 4]) -> String {
    let Source {
        repo,
        rev,
        netlist_sha256,
        ..
    } = &sidecar.source;
    let [sda, scl, irq, ven] = gpio_pins;
    let addr = sidecar.nfc.i2c_addr;
    format!(
        r#"//! @generated from {repo}@{rev} {netlist_path} (netlist sha256 {netlist_sha256})
//! regenerate: cargo run -p boards-gen -- --board nucula
//! DO NOT EDIT: hand edits are overwritten and fail the boards-consistency
//! check (design §5). Sources beyond the netlist are cited inline; the
//! bench-proven cross-check lives in the tests below.

use crate::schema::*;

/// nucula — ESP32-C3 + PN7160 (zeugmaster hardware repo, rev-B-draft).
/// Every GPIO number below is extracted from the pinned netlist by the
/// sidecar-anchored generator; the bench constants they MUST equal live
/// in `crates/pn7160-nci/src/board.rs` (see the tests).
pub const NUCULA: BoardDesc = BoardDesc {{
    // ---- identity ----
    name: "nucula",
    chip: Chip::Esp32c3, // → target riscv32imc-esp-espidf
    variant_note: Some(
        "rev-B-draft hardware repo @{rev}; R23/R24 strap the NCI address — nominal {addr:#x}, \
         probe before assuming (strap margin confirmed on the bench, #62)",
    ),

    // ---- buses ----
    buses: &[Bus {{
        role: BusRole::NfcFrontend,
        kind: BusKind::I2c(I2cBus {{
            port: 0,
            sda: {sda}, // netlist net {sda_net}: {nfc_ref} {sda_pin} ↔ {mcu_ref} IO{sda}
            scl: {scl}, // netlist net {scl_net}: {nfc_ref} {scl_pin} ↔ {mcu_ref} IO{scl}
            // No separate probe speed: driver_ng probes at bus level.
            // Run speed is the shared-bus limit — the PCF8574T keyboard
            // expander caps I2C at 100 kHz (pn7160-nci board.rs rule).
            hz_probe: 0,
            hz_run: 100_000,
        }}),
    }}],

    // ---- named pin roles (netlist nets {irq_net} / {ven_net}) ----
    pins: &[
        Pin {{ role: "nfc_irq", gpio: {irq} }}, // {nfc_ref} IRQ ↔ {mcu_ref} IO{irq}
        Pin {{ role: "nfc_ven", gpio: {ven} }}, // {nfc_ref} VEN ↔ {mcu_ref} IO{ven}
    ],

    // ---- NFC peripheral on the frontend bus ----
    nfc: Some(NfcPeripheral {{
        controller: NfcController::Pn7160,
        // Strap-derived nominal (R23/R24) — bench-proven constant, NOT a
        // netlist fact; equality with pn7160-nci::board::NUCULA is
        // asserted below so drift fails CI.
        i2c_addr: {addr},
    }}),

    // ---- capabilities ----
    caps: Caps {{
        lcd: Cap::None,        // SSD1309 OLED @0x3C exists on hw (sheet 4) — no
                               // firmware capability yet
        led: Cap::None,        // no status LED
        axp192_pmu: Cap::None, // no AXP192 (dedicated PMU rails instead)
        ble: Cap::Feature("ble"),            // build.sh c3-ccid-ble
        spp: Cap::None,                       // ESP32-C3 has no BR/EDR classic
        wifi_vpcd: Cap::Feature("wifi-vpcd"), // build.sh wifi-vpcd (nucula leg)
        wired_contact: false,                 // NFC only
    }},

    // ---- console + wire ----
    console: Console {{
        kind: ConsoleKind::UsbCdc, // ROM USB-Serial/JTAG on GPIO18/19
        baud: 115_200,
        stopbits: 1, // CDC 8N1 (esp32_flash.py — vs the GemPC 8N2 UART)
    }},
    wire: Wire::GemPcSerial, // GemPC Twin emulation over the CDC console

    // ---- flash/OTA family (design §6) ----
    ota_partition_family: OtaFamily::EspIdfOta {{
        csv: "firmware/esp32-ccid/partitions-ota.csv", // build.sh c3 FLASH_OFFSET
        app_slot_offset: 0x40000,                      // ota_0 slot per the CSV
        bootloader_offset: 0x0,                        // esp32c3/riscv32 rule (§6)
    }},

    // ---- provenance ----
    provenance: "GENERATED from {repo}@{rev} docs/netlist.xml (sha256 {short_digest}…); bench cross-check crates/pn7160-nci/src/board.rs (addr {addr:#x}, sda {sda}, scl {scl}, irq {irq}, ven {ven}, I2C ≤100 kHz PCF8574T); console esp32_flash.py CDC 8N1; OTA build.sh c3 + partitions-ota.csv",
}};

/// `board-nucula` — the Cargo feature that selects this table entry.
pub const FEATURE: &str = "board-nucula";

#[cfg(test)]
mod tests {{
    use super::*;

    /// The §2.1 landing cross-check: the GENERATED table equals the
    /// historical bench-proven constants — drift is impossible, not
    /// merely discouraged. (pn7160-nci's own board.rs tests stay
    /// untouched: no rev bump forced on bolty.)
    #[test]
    fn generated_pn7160_block_equals_pn7160_nci_board() {{
        let bench = pn7160_nci::board::NUCULA;
        let bus = NUCULA.bus(BusRole::NfcFrontend).i2c();
        assert_eq!(bus.sda as i32, bench.sda, "sda: netlist vs bench");
        assert_eq!(bus.scl as i32, bench.scl, "scl: netlist vs bench");
        assert_eq!(
            NUCULA.pin("nfc_irq") as i32,
            bench.irq,
            "irq: netlist vs bench"
        );
        assert_eq!(
            NUCULA.pin("nfc_ven") as i32,
            bench.ven,
            "ven: netlist vs bench"
        );
        assert_eq!(
            NUCULA.nfc.expect("nucula: NFC frontend").i2c_addr,
            bench.addr,
            "i2c addr: strap sidecar vs bench"
        );
    }}

    /// Per-field bench constants, spelled out (the pn7160-nci test
    /// pattern): if the netlist disagrees, the bench wins and this test
    /// names the finding against the netlist revision.
    #[test]
    fn nucula_matches_the_historical_bench_constants() {{
        let bus = NUCULA.bus(BusRole::NfcFrontend).i2c();
        assert_eq!((bus.port, bus.sda, bus.scl), (0, {sda}, {scl}));
        assert_eq!(
            (NUCULA.pin("nfc_irq"), NUCULA.pin("nfc_ven")),
            ({irq}, {ven})
        );
        assert_eq!(NUCULA.nfc.unwrap().i2c_addr, {addr});
        // Shared-bus rule: the PCF8574T caps the bus at 100 kHz.
        assert_eq!(bus.hz_run, 100_000);
    }}

    #[test]
    fn identity_and_family() {{
        assert_eq!(NUCULA.name, "nucula");
        assert_eq!(NUCULA.chip, Chip::Esp32c3);
        assert_eq!(NUCULA.chip.target(), Some("riscv32imc-esp-espidf"));
        assert_eq!(FEATURE, "board-nucula");
        assert!(crate::ALL_BOARDS.iter().any(|b| b.name == "nucula"));
        let OtaFamily::EspIdfOta {{
            csv,
            app_slot_offset,
            bootloader_offset,
        }} = NUCULA.ota_partition_family
        else {{
            panic!("nucula is an esp-idf OTA board");
        }};
        assert_eq!(csv, "firmware/esp32-ccid/partitions-ota.csv");
        assert_eq!((app_slot_offset, bootloader_offset), (0x40000, 0x0));
    }}
}}
"#,
        repo = repo,
        rev = rev,
        netlist_path = sidecar.source.netlist_path,
        netlist_sha256 = netlist_sha256,
        short_digest = &netlist_sha256[..16],
        addr = addr,
        sda = sda,
        scl = scl,
        irq = irq,
        ven = ven,
        sda_net = gpio.sda.net,
        scl_net = gpio.scl.net,
        irq_net = gpio.irq.net,
        ven_net = gpio.ven.net,
        nfc_ref = sidecar.anchors.nfc_ref,
        mcu_ref = sidecar.anchors.mcu_ref,
        sda_pin = gpio.sda.pin,
        scl_pin = gpio.scl.pin,
    )
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    let mut board = String::new();
    let mut netlist: Option<String> = None;
    let mut out: Option<String> = None;
    let mut check_only = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--board" => board = args.next().unwrap_or_default(),
            "--netlist" => netlist = args.next(),
            "--out" => out = args.next(),
            "--check" => check_only = true,
            other => {
                eprintln!("boards-gen: unknown arg {other:?}");
                eprintln!(
                    "usage: cargo run -p boards-gen -- --board nucula \
                     [--netlist PATH] [--out PATH] [--check]"
                );
                std::process::exit(2);
            }
        }
    }
    if board != "nucula" {
        eprintln!("boards-gen: --board {board:?} not supported (only nucula)");
        std::process::exit(2);
    }

    // Paths anchor at the repo root (two levels above this crate).
    let manifest_dir = std::env!("CARGO_MANIFEST_DIR");
    let root = std::path::Path::new(manifest_dir)
        .ancestors()
        .nth(2)
        .expect("tools/boards-gen -> repo root")
        .to_path_buf();
    let sidecar_path = root.join("crates/boards/gen/nucula-nets.toml");
    let out_path = out
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("crates/boards/src/nucula_gen.rs"));

    let sidecar_text = std::fs::read_to_string(&sidecar_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", sidecar_path.display()));
    let sidecar: Sidecar =
        toml::from_str(&sidecar_text).unwrap_or_else(|e| panic!("parse {}: {e}", sidecar_path.display()));

    let bytes = match fetch_netlist(&sidecar, netlist.as_deref()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("boards-gen: {e}");
            std::process::exit(1);
        }
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let nets = parse_nets(&text);

    let mut gpio_pins = [0u8; 4];
    for (slot, (label, anchor)) in [
        ("sda", &sidecar.nfc.nets.sda),
        ("scl", &sidecar.nfc.nets.scl),
        ("irq", &sidecar.nfc.nets.irq),
        ("ven", &sidecar.nfc.nets.ven),
    ]
    .into_iter()
    .enumerate()
    {
        let Some(net) = nets.iter().find(|n| n.name == anchor.net) else {
            eprintln!(
                "boards-gen: net {} not found — upstream rename? update the sidecar",
                anchor.net
            );
            std::process::exit(1);
        };
        gpio_pins[slot] =
            match anchor_gpio(net, &sidecar.anchors.nfc_ref, &sidecar.anchors.mcu_ref, &anchor.pin)
            {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("boards-gen: {label}: {e}");
                    std::process::exit(1);
                }
            };
    }
    println!(
        "boards-gen: nucula sda={} scl={} irq={} ven={} addr={:#04x} (from {}@{})",
        gpio_pins[0],
        gpio_pins[1],
        gpio_pins[2],
        gpio_pins[3],
        sidecar.nfc.i2c_addr,
        sidecar.source.repo,
        &sidecar.source.rev[..8],
    );

    let generated = emit(&sidecar, &sidecar.nfc.nets, gpio_pins);
    if check_only {
        match std::fs::read_to_string(&out_path) {
            Ok(current) if current == generated => {
                println!("boards-gen: {} up to date", out_path.display());
            }
            Ok(_) => {
                eprintln!(
                    "boards-gen: {} DRIFTED from sidecar+netlist — regenerate \
                     (cargo run -p boards-gen -- --board nucula)",
                    out_path.display()
                );
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("boards-gen: read {}: {e}", out_path.display());
                std::process::exit(1);
            }
        }
    } else {
        std::fs::write(&out_path, &generated)
            .unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));
        println!("boards-gen: wrote {}", out_path.display());
    }
}

// ---------------------------------------------------------------------------
// fixture tests (renamed refs + error paths — the scanner is load-bearing)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"<export version="E">
  <nets>
    <net code="3" name="/I2C_SCL" class="Default">
      <node ref="U2" pin="7" pinfunction="I2C_SCL_7" pintype="bidirectional"/>
      <node ref="U1" pin="19" pinfunction="IO5_19" pintype="bidirectional"/>
    </net>
    <net code="4" name="/I2C_SDA" class="Default">
      <node ref="U2" pin="5" pinfunction="I2C_SDA_5" pintype="bidirectional"/>
      <node ref="U1" pin="18" pinfunction="IO4_18" pintype="bidirectional"/>
    </net>
    <net code="32" name="/NFC_IRQ" class="Default">
      <node ref="U2" pin="8" pinfunction="IRQ_8" pintype="output"/>
      <node ref="U1" pin="20" pinfunction="IO6_20" pintype="input"/>
    </net>
    <net code="59" name="GND" class="Default">
      <node ref="U2" pin="9" pinfunction="VSS_9" pintype="power_in"/>
      <node ref="U1" pin="1" pinfunction="GND_1" pintype="power_in"/>
    </net>
  </nets>
</export>"#;

    #[test]
    fn extracts_gpio_from_fixture() {
        let nets = parse_nets(FIXTURE);
        let scl = nets.iter().find(|n| n.name == "/I2C_SCL").unwrap();
        assert_eq!(anchor_gpio(scl, "U2", "U1", "I2C_SCL").unwrap(), 5);
        let sda = nets.iter().find(|n| n.name == "/I2C_SDA").unwrap();
        assert_eq!(anchor_gpio(sda, "U2", "U1", "I2C_SDA").unwrap(), 4);
        let irq = nets.iter().find(|n| n.name == "/NFC_IRQ").unwrap();
        assert_eq!(anchor_gpio(irq, "U2", "U1", "IRQ").unwrap(), 6);
    }

    #[test]
    fn anchor_must_exist_on_the_nfc_side() {
        let nets = parse_nets(FIXTURE);
        let gnd = nets.iter().find(|n| n.name == "GND").unwrap();
        assert!(anchor_gpio(gnd, "U2", "U1", "IRQ").is_err());
    }

    #[test]
    fn attr_extraction() {
        assert_eq!(
            attr(r#"<node ref="U6" pin="8" pinfunction="IRQ_8"/>"#, "ref"),
            Some("U6")
        );
        assert_eq!(attr(r#"<node ref="U6" pin="8"/>"#, "pinfunction"), None);
    }

    /// The real pinned netlist, if dropped next to the crate by the
    /// developer, must yield the bench constants (the sha256 pin is
    /// enforced by fetch_netlist in the run path; here we pin only the
    /// extraction). Skipped when the fixture file is absent.
    #[test]
    fn real_netlist_extraction_when_present() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/nucula-netlist.xml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("skipping: {} absent", path.display());
            return;
        };
        let nets = parse_nets(&text);
        for (net_name, anchor, expect) in [
            ("/I2C_SDA", "I2C_SDA", 4),
            ("/I2C_SCL", "I2C_SCL", 5),
            ("/NFC_IRQ", "IRQ", 6),
            ("/NFC_VEN", "VEN", 7),
        ] {
            let net = nets.iter().find(|n| n.name == net_name).unwrap();
            assert_eq!(anchor_gpio(net, "U6", "U3", anchor).unwrap(), expect);
        }
    }
}
