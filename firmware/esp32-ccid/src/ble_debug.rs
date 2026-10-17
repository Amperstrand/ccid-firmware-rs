//! BLE GATT debug console server (issue #66).
//!
//! Advertises a Nordic UART Service (NUS) lookalike — service
//! `6E400001-…`, one Notify characteristic on `6E400003-…` — so stock
//! centrals (nRF Connect, Serial Bluetooth Terminal, bleak /
//! `tests/hardware/ble/ble_log_capture.py`) attach without custom
//! tooling. Logging rides GATT notifications instead of the USB-CDC
//! console, keeping the CCID wire clean.
//!
//! Real implementation on any ESP target (xtensa classic ESP32 or
//! riscv32 ESP32-C3 — both have Bluetooth) built with the `ble`
//! feature; a no-op stub elsewhere so host tests compile. Requires
//! `CONFIG_BT_ENABLED=y` + `CONFIG_BT_BLUEDROID_ENABLED=y` in the
//! sdkconfig (the `sdkconfig-*-ble.full` bench variants) — a `ble`
//! feature build against a BT-disabled sdkconfig fails at compile
//! time, which is the loud failure we want.

#[cfg(all(any(target_arch = "xtensa", target_arch = "riscv32"), feature = "ble"))]
mod imp {
    use std::sync::{Arc, Mutex};

    use esp_idf_svc::bt::ble::gap::{AdvConfiguration, BleGapEvent, EspBleGap};
    use esp_idf_svc::bt::ble::gatt::server::{ConnectionId, EspGatts, GattsEvent, TransferId};
    use esp_idf_svc::bt::ble::gatt::{
        AutoResponse, GattCharacteristic, GattDescriptor, GattId, GattInterface, GattServiceId,
        GattStatus, Handle, Permission, Property,
    };
    use esp_idf_svc::bt::{BdAddr, Ble, BtDriver, BtStatus, BtUuid};
    use esp_idf_sys::{EspError, ESP_FAIL};

    pub const APP_ID: u16 = 0;
    pub const DEVICE_NAME: &str = "ESP32-CCID-Debug";
    /// Nordic UART Service (NUS) service UUID, canonical (big-endian)
    /// u128 form; `BtUuid::uuid128` applies the LSB-first on-air order.
    pub const SERVICE_UUID: u128 = 0x6E400001_B5A3_F393_E0A9_E50E24DCCA9E;
    /// NUS TX characteristic (peripheral → central notifications).
    pub const LOG_CHAR_UUID: u128 = 0x6E400003_B5A3_F393_E0A9_E50E24DCCA9E;

    const CCCD_UUID: u16 = 0x2902;
    const DEFAULT_MTU: usize = 23;
    const GATT_NOTIFY_OVERHEAD: usize = 3;
    const MAX_LOG_CHUNK: usize = 200;

    type GapHandle = Arc<EspBleGap<'static, Ble, Arc<BtDriver<'static, Ble>>>>;
    type GattsHandle = Arc<EspGatts<'static, Ble, Arc<BtDriver<'static, Ble>>>>;

    #[derive(Debug, Clone)]
    pub struct Connection {
        pub peer: BdAddr,
        pub conn_id: Handle,
        pub notify_enabled: bool,
        pub mtu: usize,
    }

    #[derive(Default)]
    pub struct BleState {
        pub gatt_if: Option<GattInterface>,
        pub service_handle: Option<Handle>,
        pub log_handle: Option<Handle>,
        cccd_handle: Option<Handle>,
        pub connections: Vec<Connection>,
    }

    #[derive(Clone)]
    pub struct BleDebugServer {
        pub gap: GapHandle,
        pub gatts: GattsHandle,
        pub state: Arc<Mutex<BleState>>,
    }

    impl BleDebugServer {
        pub fn new(gap: GapHandle, gatts: GattsHandle) -> Self {
            Self {
                gap,
                gatts,
                state: Arc::new(Mutex::new(BleState::default())),
            }
        }

        fn lock_state(&self) -> std::sync::MutexGuard<'_, BleState> {
            self.state.lock().unwrap_or_else(|e| {
                log::warn!("BLE state lock poisoned — recovering");
                e.into_inner()
            })
        }

        pub fn subscribe(&self) -> Result<(), EspError> {
            let gap_server = self.clone();
            self.gap.subscribe(move |event| {
                gap_server.check_esp_status(gap_server.on_gap_event(event));
            })?;

            let gatts_server = self.clone();
            self.gatts.subscribe(move |(gatt_if, event)| {
                gatts_server.check_esp_status(gatts_server.on_gatts_event(gatt_if, event));
            })?;

            Ok(())
        }

        pub fn register_app(&self) -> Result<(), EspError> {
            self.gatts.register_app(APP_ID)
        }

        pub fn has_subscribers(&self) -> bool {
            self.lock_state()
                .connections
                .iter()
                .any(|connection| connection.notify_enabled)
        }

        pub fn send_log(&self, line: &str) -> bool {
            self.send_log_bytes(line.as_bytes())
        }

        pub fn send_log_bytes(&self, data: &[u8]) -> bool {
            if data.is_empty() {
                return true;
            }

            let mut delivered = false;
            let mut state = self.lock_state();

            let Some(gatt_if) = state.gatt_if else {
                return false;
            };
            let Some(log_handle) = state.log_handle else {
                return false;
            };

            for connection in &mut state.connections {
                if !connection.notify_enabled {
                    continue;
                }

                let mtu_payload = connection
                    .mtu
                    .saturating_sub(GATT_NOTIFY_OVERHEAD)
                    .clamp(1, MAX_LOG_CHUNK);

                let mut notify_failed = false;

                for chunk in data.chunks(mtu_payload) {
                    if self
                        .gatts
                        .notify(gatt_if, connection.conn_id, log_handle, chunk)
                        .is_err()
                    {
                        notify_failed = true;
                        break;
                    }

                    delivered = true;
                }

                if notify_failed {
                    connection.notify_enabled = false;
                }
            }

            delivered
        }

        /// On-target identity source: this chip's factory BT MAC with
        /// the static-random top bits forced (see
        /// [`crate::ble_addr::static_random_from_mac`])..
        fn static_random_address() -> [u8; 6] {
            let mut mac = [0u8; 6];
            let rc = unsafe {
                esp_idf_sys::esp_read_mac(mac.as_mut_ptr(), esp_idf_sys::esp_mac_type_t_ESP_MAC_BT)
            };
            if rc != 0 {
                println!("ble: esp_read_mac(BT) FAILED: esp_err={rc} — advertising with zero MAC");
            }
            crate::ble_addr::static_random_from_mac(mac)
        }

        fn on_gap_event(&self, event: BleGapEvent) -> Result<(), EspError> {
            // #165 diagnostic: print the REAL BtStatus (Debug) per
            // advertising event — `check_bt_status` collapses every
            // non-Success to a bare ESP_FAIL, which is how the 0x0c
            // (Command Disallowed) label got lost (it came from an
            // older unsilenced build). Logging only, no behavior delta.
            // Gated on `ble-diag` (Codex P1 on #205): these events fire
            // asynchronously, and a raw println after the transport is
            // claimed would put unsolicited ASCII on a CCID-shared
            // console — clean-wire violation in every standard `ble`
            // build.
            let diag = cfg!(feature = "ble-diag");
            match event {
                BleGapEvent::AdvertisingConfigured(status) => {
                    if diag {
                        println!("ble: AdvertisingConfigured status={status:?}");
                    }
                    self.check_bt_status(status)?
                }
                BleGapEvent::AdvertisingStarted(status) => {
                    if diag {
                        println!("ble: AdvertisingStarted status={status:?}");
                    }
                    self.check_bt_status(status)?
                }
                BleGapEvent::AdvertisingStopped(status) => {
                    if diag {
                        println!("ble: AdvertisingStopped status={status:?}");
                    }
                    self.check_bt_status(status)?
                }
                _ => {}
            }

            // Canonical Bluedroid bring-up chain (mirrors the IDF
            // gatt_server example): ADV-data write completes → write the
            // SCAN_RSP → its completion is the only safe point to start
            // advertising. Starting on the first completion races the
            // scan-rsp write at the HCI level — the C3 controller answers
            // LE Set Advertising Parameters with Command Disallowed (0x0c)
            // and never advertises (bench 2026-10-09).
            if let BleGapEvent::AdvertisingConfigured(status) = event {
                self.check_bt_status(status)?;
                self.set_scan_rsp_conf()?;
            }

            if let BleGapEvent::ScanResponseConfigured(status) = event {
                self.check_bt_status(status)?;
                println!("ble: adv + scan rsp configured — starting advertising");
                // Static-random address set HERE, not at bring-up: the
                // bench-verified bring-up-time call returns rc=0 but the
                // controller reports "No random address yet" at adv start
                // (AdvertisingStarted status=InvalidParam) — the stack is
                // not ready to accept it before the GATT chain runs. HCI
                // commands serialize, so the random-address write completes
                // before the adv-start is processed (the ESP-IDF ibeacon
                // example uses the same set-then-start sequence).
                // ESP-IDF bd_addr arrays are MSB-FIRST (displayed order,
                // bench-verified: the reversed order drew
                // "BT_BTC: Invalid random address type" — the controller
                // checks the FIRST array byte for the static-random top
                // bits). Static random: MSB top bits 11. The address is
                // DERIVED from this chip's factory BT MAC (Codex P2 on
                // #231): every unit gets a distinct on-air identity
                // instead of one constant per architecture.
                let mut rand_addr = static_random_address();
                println!(
                    "ble: static-random addr {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X} (from factory MAC)",
                    rand_addr[0], rand_addr[1], rand_addr[2],
                    rand_addr[3], rand_addr[4], rand_addr[5],
                );
                let rc = unsafe { esp_idf_sys::esp_ble_gap_set_rand_addr(rand_addr.as_mut_ptr()) };
                if rc != 0 {
                    println!("ble: set_rand_addr FAILED: esp_err={rc}");
                }
                // esp-idf-svc's start_advertising hardcodes
                // own_addr_type = PUBLIC (esp-idf-svc 0.53 gap.rs:1082, a
                // literal TODO) — our static-random address would never be
                // used. Same params as the svc call, but own_addr_type =
                // RANDOM (BLE_ADDR_TYPE_RANDOM = 0x01, esp_bt_defs.h).
                let mut adv_param = esp_idf_sys::esp_ble_adv_params_t {
                    adv_int_min: 0x20,
                    adv_int_max: 0x40,
                    adv_type: 0x00,      // ADV_TYPE_IND
                    own_addr_type: 0x01, // BLE_ADDR_TYPE_RANDOM
                    peer_addr: [0; 6],
                    peer_addr_type: 0x00,    // BLE_ADDR_TYPE_PUBLIC
                    channel_map: 0x07,       // ADV_CHNL_ALL
                    adv_filter_policy: 0x00, // ADV_FILTER_ALLOW_SCAN_ANY_CON_ANY
                };
                let rc2 = unsafe { esp_idf_sys::esp_ble_gap_start_advertising(&mut adv_param) };
                if rc2 != 0 {
                    println!("ble: start_advertising FAILED: esp_err={rc2}");
                }
            }

            Ok(())
        }

        fn on_gatts_event(
            &self,
            gatt_if: GattInterface,
            event: GattsEvent,
        ) -> Result<(), EspError> {
            match event {
                GattsEvent::ServiceRegistered { status, app_id } => {
                    self.check_gatt_status(status)?;
                    if app_id == APP_ID {
                        self.create_service(gatt_if)?;
                    }
                }
                GattsEvent::ServiceCreated {
                    status,
                    service_handle,
                    ..
                } => {
                    self.check_gatt_status(status)?;
                    self.configure_and_start_service(service_handle)?;
                }
                GattsEvent::CharacteristicAdded {
                    status,
                    attr_handle,
                    service_handle,
                    char_uuid,
                } => {
                    self.check_gatt_status(status)?;
                    self.register_characteristic(service_handle, attr_handle, char_uuid)?;
                }
                GattsEvent::DescriptorAdded {
                    status,
                    attr_handle,
                    service_handle,
                    descr_uuid,
                } => {
                    self.check_gatt_status(status)?;
                    self.register_descriptor(service_handle, attr_handle, descr_uuid)?;
                }
                GattsEvent::PeerConnected { conn_id, addr, .. } => {
                    self.create_conn(conn_id, addr)?;
                }
                GattsEvent::PeerDisconnected { addr, .. } => {
                    self.delete_conn(addr)?;
                }
                GattsEvent::Write {
                    conn_id,
                    trans_id,
                    handle,
                    offset,
                    need_rsp,
                    is_prep,
                    value,
                    ..
                } => {
                    self.handle_write(
                        gatt_if, conn_id, trans_id, handle, offset, need_rsp, is_prep, value,
                    )?;
                }
                GattsEvent::Mtu { conn_id, mtu } => {
                    self.register_conn_mtu(conn_id, mtu)?;
                }
                _ => {}
            }

            Ok(())
        }

        fn set_adv_conf(&self) -> Result<(), EspError> {
            // The 31-byte ADV payload fits flags + 128-bit UUID + TX power;
            // the device name (19 B) only fits the scan response (written
            // on AdvertisingConfigured — see on_gap_event for the chain).
            self.gap.set_adv_conf(&AdvConfiguration {
                include_txpower: true,
                flag: 2,
                service_uuid: Some(BtUuid::uuid128(SERVICE_UUID)),
                ..Default::default()
            })
        }

        fn set_scan_rsp_conf(&self) -> Result<(), EspError> {
            self.gap.set_adv_conf(&AdvConfiguration {
                set_scan_rsp: true,
                include_name: true,
                ..Default::default()
            })
        }

        fn create_service(&self, gatt_if: GattInterface) -> Result<(), EspError> {
            self.lock_state().gatt_if = Some(gatt_if);

            self.gap.set_device_name(DEVICE_NAME)?;
            self.set_adv_conf()?;
            self.gatts.create_service(
                gatt_if,
                &GattServiceId {
                    id: GattId {
                        uuid: BtUuid::uuid128(SERVICE_UUID),
                        inst_id: 0,
                    },
                    is_primary: true,
                },
                4,
            )?;

            Ok(())
        }

        fn configure_and_start_service(&self, service_handle: Handle) -> Result<(), EspError> {
            self.lock_state().service_handle = Some(service_handle);
            self.gatts.start_service(service_handle)?;
            self.gatts.add_characteristic(
                service_handle,
                &GattCharacteristic {
                    uuid: BtUuid::uuid128(LOG_CHAR_UUID),
                    permissions: Permission::Read.into(),
                    properties: Property::Notify.into(),
                    max_len: MAX_LOG_CHUNK,
                    auto_rsp: AutoResponse::ByApp,
                },
                &[],
            )?;

            Ok(())
        }

        fn register_characteristic(
            &self,
            service_handle: Handle,
            attr_handle: Handle,
            char_uuid: BtUuid,
        ) -> Result<(), EspError> {
            let should_add_cccd = {
                let mut state = self.lock_state();

                if state.service_handle != Some(service_handle)
                    || char_uuid != BtUuid::uuid128(LOG_CHAR_UUID)
                {
                    false
                } else {
                    state.log_handle = Some(attr_handle);
                    true
                }
            };

            if should_add_cccd {
                self.gatts.add_descriptor(
                    service_handle,
                    &GattDescriptor {
                        uuid: BtUuid::uuid16(CCCD_UUID),
                        permissions: Permission::Read | Permission::Write,
                    },
                )?;
            }

            Ok(())
        }

        fn register_descriptor(
            &self,
            service_handle: Handle,
            attr_handle: Handle,
            descr_uuid: BtUuid,
        ) -> Result<(), EspError> {
            let mut state = self.lock_state();

            if state.service_handle == Some(service_handle)
                && descr_uuid == BtUuid::uuid16(CCCD_UUID)
            {
                state.cccd_handle = Some(attr_handle);
            }

            Ok(())
        }

        fn create_conn(&self, conn_id: ConnectionId, addr: BdAddr) -> Result<(), EspError> {
            {
                let mut state = self.lock_state();

                if let Some(existing) = state.connections.iter_mut().find(|conn| conn.peer == addr)
                {
                    existing.conn_id = conn_id;
                    existing.notify_enabled = false;
                    existing.mtu = DEFAULT_MTU;
                } else {
                    state.connections.push(Connection {
                        peer: addr,
                        conn_id,
                        notify_enabled: false,
                        mtu: DEFAULT_MTU,
                    });
                }
            }

            self.set_adv_conf()?;
            self.gap.set_conn_params_conf(addr, 10, 20, 0, 400)?;

            Ok(())
        }

        fn delete_conn(&self, addr: BdAddr) -> Result<(), EspError> {
            {
                let mut state = self.lock_state();
                state
                    .connections
                    .retain(|connection| connection.peer != addr);
            }

            self.set_adv_conf()?;

            Ok(())
        }

        fn register_conn_mtu(&self, conn_id: ConnectionId, mtu: u16) -> Result<(), EspError> {
            let mut state = self.lock_state();

            if let Some(connection) = state
                .connections
                .iter_mut()
                .find(|connection| connection.conn_id == conn_id)
            {
                connection.mtu = usize::from(mtu);
            }

            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn handle_write(
            &self,
            gatt_if: GattInterface,
            conn_id: ConnectionId,
            trans_id: TransferId,
            handle: Handle,
            offset: u16,
            need_rsp: bool,
            _is_prep: bool,
            value: &[u8],
        ) -> Result<(), EspError> {
            let handled = {
                let mut state = self.lock_state();
                let cccd_handle = state.cccd_handle;

                if Some(handle) != cccd_handle || offset != 0 || value.len() < 2 {
                    false
                } else if let Some(connection) = state
                    .connections
                    .iter_mut()
                    .find(|connection| connection.conn_id == conn_id)
                {
                    let cccd_value = u16::from_le_bytes([value[0], value[1]]);
                    connection.notify_enabled = (cccd_value & 0x0001) != 0;
                    true
                } else {
                    false
                }
            };

            if handled && need_rsp {
                self.gatts
                    .send_response(gatt_if, conn_id, trans_id, GattStatus::Ok, None)?;
            }

            Ok(())
        }

        fn check_esp_status(&self, status: Result<(), EspError>) {
            // Raw println!: the BLE log sink may itself be what's failing;
            // event-chain errors must surface on the console.
            if let Err(e) = status {
                println!("ble: event-chain error: {e}");
            }
        }

        fn check_bt_status(&self, status: BtStatus) -> Result<(), EspError> {
            if matches!(status, BtStatus::Success) {
                Ok(())
            } else {
                Err(EspError::from_infallible::<ESP_FAIL>())
            }
        }

        fn check_gatt_status(&self, status: GattStatus) -> Result<(), EspError> {
            if matches!(status, GattStatus::Ok) {
                Ok(())
            } else {
                Err(EspError::from_infallible::<ESP_FAIL>())
            }
        }
    }
}

#[cfg(not(all(any(target_arch = "xtensa", target_arch = "riscv32"), feature = "ble")))]
mod imp {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Connection;

    #[derive(Default)]
    pub struct BleState;

    #[derive(Clone, Default)]
    pub struct BleDebugServer;

    pub const APP_ID: u16 = 0;
    pub const DEVICE_NAME: &str = "ESP32-CCID-Debug";
    pub const SERVICE_UUID: u128 = 0x6E400001_B5A3_F393_E0A9_E50E24DCCA9E;
    pub const LOG_CHAR_UUID: u128 = 0x6E400003_B5A3_F393_E0A9_E50E24DCCA9E;

    impl BleDebugServer {
        pub fn has_subscribers(&self) -> bool {
            false
        }

        pub fn send_log(&self, _line: &str) -> bool {
            false
        }

        pub fn send_log_bytes(&self, _data: &[u8]) -> bool {
            false
        }
    }
}

pub use imp::*;
