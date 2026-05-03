#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
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
    pub const SERVICE_UUID: u128 = 0x8f4211d65b444e8c9c7a7f0f4e8d0001;
    pub const LOG_CHAR_UUID: u128 = 0x8f4211d65b444e8c9c7a7f0f4e8d0002;

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

        fn on_gap_event(&self, event: BleGapEvent) -> Result<(), EspError> {
            match event {
                BleGapEvent::AdvertisingConfigured(status)
                | BleGapEvent::AdvertisingStarted(status)
                | BleGapEvent::AdvertisingStopped(status) => self.check_bt_status(status)?,
                _ => {}
            }

            if let BleGapEvent::AdvertisingConfigured(status) = event {
                self.check_bt_status(status)?;
                self.gap.start_advertising()?;
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
            self.gap.set_adv_conf(&AdvConfiguration {
                include_name: true,
                include_txpower: true,
                flag: 2,
                service_uuid: Some(BtUuid::uuid128(SERVICE_UUID)),
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
            let _ = status;
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

#[cfg(not(all(target_arch = "xtensa", feature = "backend-mfrc522")))]
mod imp {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Connection;

    #[derive(Default)]
    pub struct BleState;

    #[derive(Clone, Default)]
    pub struct BleDebugServer;

    pub const APP_ID: u16 = 0;
    pub const DEVICE_NAME: &str = "ESP32-CCID-Debug";
    pub const SERVICE_UUID: u128 = 0x8f4211d65b444e8c9c7a7f0f4e8d0001;
    pub const LOG_CHAR_UUID: u128 = 0x8f4211d65b444e8c9c7a7f0f4e8d0002;

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
