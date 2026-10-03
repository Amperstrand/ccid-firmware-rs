//! Typed WiFi station manager, ported from the proven
//! micronuts-esp32-wallet / bolty-rs pattern: BlockingWifi<EspWifi>,
//! deterministic lifecycle (disconnect → stop → configure → start →
//! connect → bounded netif poll — never an unbounded event wait).

use core::fmt;

use esp_idf_hal::modem::Modem;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    nvs::EspDefaultNvsPartition,
    wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};
use esp_idf_sys::EspError;

const MAX_SSID_LEN: usize = 32;
const MAX_PASSWORD_LEN: usize = 64;

#[derive(Debug)]
pub enum WifiError {
    SsidTooLong,
    PasswordTooLong,
    NoLease,
    Esp(EspError),
}

impl fmt::Display for WifiError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::SsidTooLong => f.write_str("ssid too long"),
            Self::PasswordTooLong => f.write_str("password too long"),
            Self::NoLease => f.write_str("no DHCP lease"),
            Self::Esp(err) => write!(f, "{err}"),
        }
    }
}

impl From<EspError> for WifiError {
    fn from(value: EspError) -> Self {
        Self::Esp(value)
    }
}

pub struct WifiManager {
    wifi: BlockingWifi<EspWifi<'static>>,
}

impl WifiManager {
    pub fn new(modem: Modem<'static>, nvs: EspDefaultNvsPartition) -> Result<Self, WifiError> {
        let sys_loop = EspSystemEventLoop::take()?;
        let wifi = BlockingWifi::wrap(EspWifi::new(modem, sys_loop.clone(), Some(nvs))?, sys_loop)?;
        Ok(Self { wifi })
    }

    /// Join `ssid` and return the station IP as a string.
    pub fn connect(&mut self, ssid: &str, password: &str) -> Result<String, WifiError> {
        if ssid.len() > MAX_SSID_LEN {
            return Err(WifiError::SsidTooLong);
        }
        if password.len() > MAX_PASSWORD_LEN {
            return Err(WifiError::PasswordTooLong);
        }

        if self.wifi.is_connected()? {
            self.wifi.disconnect()?;
        }
        if self.wifi.is_started()? {
            self.wifi.stop()?;
        }

        let wifi_configuration = Configuration::Client(ClientConfiguration {
            ssid: ssid.try_into().map_err(|_| WifiError::SsidTooLong)?,
            password: password
                .try_into()
                .map_err(|_| WifiError::PasswordTooLong)?,
            auth_method: if password.is_empty() {
                AuthMethod::None
            } else {
                AuthMethod::WPA2Personal
            },
            bssid: None,
            channel: None,
            ..Default::default()
        });

        self.wifi.set_configuration(&wifi_configuration)?;
        log::warn!("wifi: starting");
        self.wifi.start()?;
        log::warn!("wifi: associating with {}", ssid);
        self.wifi.connect()?;
        for _ in 0..60 {
            if self.wifi.is_connected().unwrap_or(false) {
                if let Ok(info) = self.wifi.wifi().sta_netif().get_ip_info() {
                    if !info.ip.is_unspecified() {
                        let ip = info.ip.to_string();
                        log::warn!("wifi: netif up, ip={}", ip);
                        return Ok(ip);
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Err(WifiError::NoLease)
    }
}
