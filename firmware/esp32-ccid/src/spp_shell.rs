//! ESP-IDF shell for the SPP transport (t_72e1e2ed extraction: split
//! out of the old `spp_server` module; the pure, host-tested pump lives
//! in `ccid_service::spp_server`). Everything below is transport
//! bring-up — BtDriver/GAP/SPP wiring — which is the firmware bin's
//! side of the crate boundary.
//!
//! Compile-loud pairing (AGENTS.md rule, same as `ble`): the `spp`
//! feature REQUIRES a classic-BT+SPP sdkconfig
//! (`sdkconfig-xtensa-spp.full`). The `esp_idf_svc::bt::spp` module
//! only exists under
//! `cfg(all(esp32, esp_idf_bt_classic_enabled, esp_idf_bt_spp_enabled))`
//! inside esp-idf-svc, so a mismatched pair fails to COMPILE at the
//! imports below — deliberately loud. Classic BR/EDR is
//! ESP32-silicon-only: there is no `c3-ccid` SPP build for the same
//! reason there is no `c3-ccid-ble`.

use std::sync::{Arc, Mutex};

use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::modem::Modem;
use esp_idf_svc::bt::gap::{DiscoveryMode, EspGap, GapEvent};
use esp_idf_svc::bt::spp::{self, EspSpp, SppConfig, SppEvent, Status};
use esp_idf_svc::bt::{reduce_bt_memory, BtClassic, BtDriver};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_sys::{
    esp, esp_bt_gap_set_security_param, esp_bt_gap_ssp_confirm_reply,
    esp_bt_sp_param_t_ESP_BT_SP_IOCAP_MODE, ESP_BT_IO_CAP_IO,
};

use crate::ccid_handler::CcidHandler;
use crate::ccid_serial_server::ResponseWindows;
use crate::nfc::NfcDriver;
use ccid_service::spp_server::{
    delivery_plan, Deliver, PumpOutcome, RxByte, SppLink, SppPump, SppRxRing,
};

/// Read-idle timeout — mirrors `UART_RX_TIMEOUT_MS` in the UART mains:
/// this long without a byte means the parser resets and the interval
/// poll fires.
const SPP_IDLE_TIMEOUT_MS: u32 = 500;
/// Card presence poll interval — mirrors `CARD_POLL_INTERVAL_MS`.
const SPP_POLL_INTERVAL_MS: u32 = 3000;
/// Serve-loop cadence: replaces the UART read-timeout wakeup; keeps the
/// pending-retry and idle paths live without busy-spinning.
const SPP_LOOP_DELAY_MS: u32 = 10;
/// Escape 0xD1 ack-before-panic settle. The UART mains call
/// `uart.flush_write()` between ack and panic; Cb-mode `esp_spp_write`
/// has no flush — give the Bluedroid task a bounded window to drain the
/// queued ack before the panic handler writes the coredump.
const SPP_SNAPSHOT_SETTLE_MS: u32 = 100;
/// RX drain granularity (entries copied out per lock hold).
const SPP_POP_BATCH: usize = 128;

/// GAP device name — matches the Escape 0x02 identity string
/// ("GemPC Twin ESP32 1.0") so both identity probes agree.
const DEVICE_NAME: &str = "GemPC Twin ESP32";
/// SPP service name (SDP "Serial Port" record).
const SERVICE_NAME: &str = "GemPC Twin SPP";

/// Shared between the Bluedroid callback task (pushes) and the serve
/// loop (pops). LOCK DISCIPLINE: the callback must never block on card
/// I/O (#89 — MFRC522 polls run hundreds of ms), so the serve loop
/// holds this mutex ONLY for memcpy-speed pops/pushes; everything slow
/// (pump, card I/O, esp_spp_write) runs outside it.
struct Shared {
    ring: SppRxRing,
    link: SppLink,
}

fn tick() -> u32 {
    unsafe { esp_idf_sys::xTaskGetTickCount() }
}

/// Bring up the SPP acceptor and serve GemPC CCID over RFCOMM.
///
/// REFERENCE (Reference-First): every bring-up step below mirrors
/// esp-rs/esp-idf-svc `examples/bt_spp_acceptor.rs` (crate 0.52.1,
/// master c8bcc9d — byte-identical SPP/GAP API): BtDriver<BtClassic> +
/// reduce_bt_memory, EspGap device name, EspSpp in Cb mode, subscribe
/// before start_server (the Init event is consumed inside
/// `EspSpp::new`), `Security::Authenticate` + `Role::Slave` + auto SCN,
/// SSP auto-confirm + legacy-PIN replies, discoverable scan mode on
/// `SppEvent::Start`.
///
/// The serving cadence mirrors `serve_uart` in main.rs byte-for-byte:
/// echo → notify → response, NAK on malformed frames, interval-gated
/// presence polls, post-response polls (Codex PR 98), read-idle parser
/// reset, Escape 0xD1 dump-and-retrieve.
pub fn serve_spp<D: NfcDriver>(mut modem: Modem<'static>, handler: CcidHandler<D>) -> ! {
    crate::install_platform_clock();
    // --- Bring-up (reference: bt_spp_acceptor.rs `main`) ---
    // `reduce_bt_memory`'s parameter is a zero-cost ownership marker;
    // this esp-idf-hal has no `reborrow` on it (the reference example
    // targets a newer hal) — `split_reborrow` yields the
    // BluetoothModem handle the marker wants, and the borrow ends
    // inside the call so `modem` still moves into BtDriver below.
    let nvs = EspDefaultNvsPartition::take().ok();
    let (_, bt_modem) = modem.split_reborrow();
    if let Err(e) = reduce_bt_memory(bt_modem) {
        log::warn!("spp: reduce_bt_memory: {e} (continuing)");
    }
    let bt = Arc::new(
        BtDriver::<BtClassic>::new(modem, nvs)
            .expect("spp: BtDriver init failed (sdkconfig-xtensa-spp.full?)"),
    );
    let gap = Arc::new(EspGap::new(bt.clone()).expect("spp: EspGap init failed"));
    gap.set_device_name(DEVICE_NAME)
        .expect("spp: set_device_name failed");
    let spp = Arc::new(
        EspSpp::new(
            bt.clone(),
            &SppConfig {
                mode: spp::Mode::Cb,
                enable_l2cap_ertm: true,
                tx_buffer_size: 0, // Cb mode (reference example)
            },
        )
        .expect("spp: EspSpp init failed"),
    );

    let shared = Arc::new(Mutex::new(Shared {
        ring: SppRxRing::new(),
        link: SppLink::new(tick()),
    }));

    // --- GAP pairing handler (reference: `on_gap_event`) ---
    let gap_cb = gap.clone();
    gap.subscribe(move |event| match event {
        GapEvent::PairingPinRequest {
            bd_addr,
            min_16_digit,
        } => {
            // Legacy pairing: reply 1234 (16-digit zeros when the host
            // demands the long form) — reference example behavior.
            let pin: &[u8] = if min_16_digit {
                &[0u8; 16]
            } else {
                &[1, 2, 3, 4]
            };
            if let Err(e) = gap_cb.reply_variable_pin(&bd_addr, Some(pin)) {
                log::warn!("spp: PIN reply failed: {e}");
            }
        }
        GapEvent::PairingUserConfirmationRequest { bd_addr, number } => {
            // SSP numeric compare: auto-accept on our side (the HOST
            // agent still shows the compare to the bench operator).
            log::info!("spp: SSP confirm {number} from {bd_addr} — accepting");
            if let Err(e) =
                esp!(unsafe { esp_bt_gap_ssp_confirm_reply(&bd_addr as *const _ as *mut _, true) })
            {
                log::warn!("spp: SSP confirm failed: {e}");
            }
        }
        GapEvent::AuthenticationCompleted {
            bd_addr, status, ..
        } => {
            log::info!("spp: auth {bd_addr}: {status:?}");
        }
        GapEvent::AclDisconnected {
            bd_addr, handle, ..
        } => {
            log::info!("spp: ACL down {bd_addr} handle {handle}");
        }
        _ => {}
    })
    .expect("spp: GAP subscribe failed");

    // --- SPP event handler (reference: `on_spp_event`) ---
    let sh = shared.clone();
    let gap_spp = gap.clone();
    spp.subscribe(move |event| {
        let mut g = sh.lock().unwrap();
        match event {
            SppEvent::Start { status, scn, .. } => {
                if status == Status::Success {
                    log::info!("spp: server started (scn {scn}) — discoverable");
                    if let Err(e) = gap_spp.set_scan_mode(true, DiscoveryMode::Discoverable) {
                        log::warn!("spp: set_scan_mode failed: {e}");
                    }
                } else {
                    log::error!("spp: start failed: {status:?}");
                }
            }
            SppEvent::ServerOpen {
                status,
                handle,
                rem_bda,
                ..
            } => {
                if status == Status::Success {
                    log::info!("spp: client {rem_bda} connected (handle {handle})");
                    // Clean-wire per connection: purge whatever the
                    // previous host left mid-frame.
                    g.link.on_open(handle, tick());
                    g.ring.clear();
                } else {
                    log::error!("spp: srv_open failed: {status:?}");
                }
            }
            SppEvent::Close {
                status,
                handle,
                async_,
                ..
            } => {
                log::info!("spp: handle {handle} closed ({status:?}, by-remote={async_})");
                g.link.on_close(tick());
                g.ring.clear();
            }
            SppEvent::Cong { cong, handle, .. } => {
                log::debug!("spp: cong={cong} handle {handle}");
                g.link.on_cong(cong);
            }
            SppEvent::DataInd {
                status,
                handle,
                length,
                data,
            } => {
                if status == Status::Success {
                    // The data pointer is valid only inside this
                    // callback (reference example) — copy under the
                    // lock, never process here (#89).
                    let bytes = unsafe { core::slice::from_raw_parts(data, length as usize) };
                    let now = tick();
                    g.link.on_rx(now);
                    g.ring.push(bytes, now);
                } else {
                    log::warn!("spp: data-ind {status:?} handle {handle}");
                }
            }
            _ => {}
        }
    })
    .expect("spp: SPP subscribe failed");

    // --- Start the acceptor (reference: `create_service`) ---
    // SCN 0 = auto-allocate (the Start event reports it; BlueZ can
    // resolve it via `sdptool browse` — bench-observed default is 1).
    spp.start_server(
        spp::Security::Authenticate,
        spp::Role::Slave,
        0,
        SERVICE_NAME,
    )
    .expect("spp: start_server failed");
    // SSP IO capability: the esp-idf-svc helper is cfg-elided (the
    // CONFIG_BT_SSP_ENABLED sdkconfig symbol no longer exists in IDF
    // 5.x) — the reference example calls the raw API.
    esp!(unsafe {
        esp_bt_gap_set_security_param(
            esp_bt_sp_param_t_ESP_BT_SP_IOCAP_MODE,
            &ESP_BT_IO_CAP_IO as *const _ as *mut std::ffi::c_void,
            1,
        )
    })
    .expect("spp: set_security_param failed");
    // Legacy-pairing fallback: variable PIN, answered in the GAP
    // handler above (reference example).
    gap.request_variable_pin()
        .expect("spp: request_variable_pin failed");

    log::warn!(
        "FWID esp32-ccid rev={} build={} transport=spp device=\"{}\"",
        env!("FW_GIT_REV"),
        env!("FW_BUILD_TS"),
        DEVICE_NAME
    );

    // --- Serve loop: serve_uart's cadence over the SPP byte pipe ---
    let poll_interval_ticks =
        esp_idf_hal::delay::TickType::new_millis(SPP_POLL_INTERVAL_MS as u64).ticks() as u32;
    // #153/#181 windows in TICKS — tick-rate-derived (Codex #158
    // P1); CONFIG_FREERTOS_HZ=100 here, so 2500/7500/59500 ms are
    // 250/750/5950 ticks.
    let response_windows = ResponseWindows::from_tick_hz(esp_idf_sys::configTICK_RATE_HZ);
    let mut pump = SppPump::new(handler, poll_interval_ticks, response_windows, tick());
    let mut batch = [RxByte { byte: 0, tick: 0 }; SPP_POP_BATCH];
    let mut retry_buf = [0u8; ccid_service::spp_server::SPP_TX_BUF_SIZE];

    loop {
        let now = tick();

        // 1. RX: copy out under the lock, serve outside it (card I/O
        //    must never stall the Bluedroid callback). `tick` is
        //    passed as the live clock: pump_rx samples it AFTER the
        //    handler ran (#153 — handler time counts, Codex #158 P1).
        let mut outcome = PumpOutcome::default();
        loop {
            let n = {
                let mut g = shared.lock().unwrap();
                g.ring.pop_batch(&mut batch)
            };
            if n == 0 {
                break;
            }
            outcome.merge(pump.pump_rx(&batch[..n], tick));
        }

        // 2. Deliver what the pump produced (echo → notify → response,
        //    or NAK) as ONE esp_spp_write.
        if outcome.any() {
            let (plan, handle) = {
                let g = shared.lock().unwrap();
                (delivery_plan(&g.link), g.link.handle())
            };
            let mut delivered = false;
            let mut parked = false;
            match plan {
                Deliver::Write => {
                    let handle = handle.expect("delivery_plan checked for a handle");
                    match spp.write(handle, pump.tx_packet_mut()) {
                        Ok(()) => delivered = true,
                        Err(e) => {
                            // Busy/full: park and retry — the host is
                            // waiting for exactly these bytes. The 0xD1
                            // intent moves INTO the packet: it can no
                            // longer outlive these bytes (Codex round
                            // 3 on #168).
                            log::debug!("spp: write busy ({e}) — parking");
                            let deadline = pump.tx_deadline();
                            let snapshot = pump.take_snapshot_request();
                            shared
                                .lock()
                                .unwrap()
                                .link
                                .park(pump.tx(), deadline, snapshot);
                            parked = true;
                        }
                    }
                }
                Deliver::Park => {
                    let deadline = pump.tx_deadline();
                    let snapshot = pump.take_snapshot_request();
                    shared
                        .lock()
                        .unwrap()
                        .link
                        .park(pump.tx(), deadline, snapshot);
                    parked = true;
                }
                Deliver::Drop => {
                    shared.lock().unwrap().link.note_dropped_tx(pump.tx().len());
                }
            }
            pump.tx_clear();

            // The 0xD1 flag must never outlive its ack: delivered →
            // consumed below; parked → rides with the bytes; neither
            // (dropped outright, e.g. no connection or TX-cap
            // rollback) → the ack is dead, cancel now.
            if !delivered && !parked {
                let _ = pump.take_snapshot_request();
            }

            // 3. Post-response cadence ONLY after delivery (#89: no
            //    physical poll may delay pending/parked bytes).
            if delivered {
                pump.record_turnaround();
                pump.poll_if_due(now);
                // Dump-and-retrieve: ack is queued — settle, panic,
                // coredump (AGENTS.md "ack-before-panic flush").
                if pump.take_snapshot_request() {
                    FreeRtos::delay_ms(SPP_SNAPSHOT_SETTLE_MS);
                    panic!("escape 0xD1: diagnostic snapshot requested");
                }
                pump.poll_if_due_after_response(tick());
            }
        }

        // 4. Pending retry: congestion cleared / stack drained — but
        //    #153 still applies: bytes whose host read window closed
        //    while they sat parked are dropped and counted, never
        //    written late (Codex review on #158, P2).
        let retry: Option<(u32, usize, bool)> = {
            let g = shared.lock().unwrap();
            match g.link.handle() {
                Some(h) if !g.link.congested() && !g.link.pending().is_empty() => {
                    let expired = g.link.pending_expired(tick());
                    let pending = g.link.pending();
                    retry_buf[..pending.len()].copy_from_slice(pending);
                    Some((h, pending.len(), expired))
                }
                _ => None,
            }
        };
        if let Some((handle, len, expired)) = retry {
            if expired {
                let mut g = shared.lock().unwrap();
                g.link.clear_pending();
                g.link.note_dropped_tx(len);
                log::debug!("spp: dropped {len} parked bytes past the host read window");
                // The 0xD1 intent (if any) died with the bytes — it
                // rides in the link, not the handler flag.
            } else if spp.write(handle, &mut retry_buf[..len]).is_ok() {
                // The parked packet reached the wire: complete its
                // dump-and-retrieve contract (if it carried a 0xD1
                // ack) exactly like the fresh-delivery path.
                let snapshot = {
                    let mut g = shared.lock().unwrap();
                    let s = g.link.take_pending_snapshot();
                    g.link.clear_pending();
                    s
                };
                if snapshot {
                    FreeRtos::delay_ms(SPP_SNAPSHOT_SETTLE_MS);
                    panic!("escape 0xD1: diagnostic snapshot requested");
                }
            }
        }

        // 5. Read-idle: no bytes for the timeout → parser reset +
        //    interval poll (mirrors the UART read-timeout arm). A ring
        //    overflow takes the same path (resync) plus diagnostics.
        let (idle_ms, overflow, dropped) = {
            let mut g = shared.lock().unwrap();
            (
                g.link.rx_idle_ms(now),
                g.ring.take_overflow(),
                g.ring.dropped() + g.link.dropped_tx() + pump.dropped_responses(),
            )
        };
        if idle_ms >= SPP_IDLE_TIMEOUT_MS || overflow {
            pump.idle(now);
        }
        if overflow || dropped > 0 {
            log::warn!("spp: rx-overflow/tx-drops total {dropped}");
        }

        FreeRtos::delay_ms(SPP_LOOP_DELAY_MS);
    }
}
