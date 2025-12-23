use std::sync::{Arc, Mutex};

use anyhow::{bail, Ok, Result};
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::peripheral,
    nvs::EspNvsPartition,
    wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};
use log::info;

/// 定义 WiFi 模块必须具备的行为
pub trait WifiStation {
    /// 连接到指定的接入点 (AP)
    /// 这是一个阻塞操作，直到连接成功或超时失败
    fn connect(&mut self, ssid: &str, password: &str) -> Result<()>;

    /// 断开连接
    fn disconnect(&mut self) -> Result<()>;

    /// 检查当前是否已连接
    fn is_connected(&self) -> Result<bool>;

    /// 获取设备的 MAC 地址 (通常用于作为 Device ID)
    fn get_mac_address(&self) -> Result<String>;

    /// 获取当前的 IP 地址 (可选，用于调试)
    fn get_ip_address(&self) -> Result<String>;

    // 如果你需要省电管理，可以加这个
    // fn set_power_save(&mut self, enabled: bool) -> Result<()>;
}

pub struct Esp32WifiDriver {
    // 使用 Arc<Mutex<>> 是为了线程安全，因为 EspWifi 可能需要在多处共享
    // 或者你可以直接持有 &mut EspWifi，取决于你的架构
    wifi: Arc<Mutex<EspWifi<'static>>>,
    sysloop: EspSystemEventLoop,
}
impl Esp32WifiDriver {
    fn new(
        modem: impl peripheral::Peripheral<P = esp_idf_svc::hal::modem::Modem> + 'static,
        sysloop: EspSystemEventLoop,
    ) -> Result<Self> {
        let nvs = EspNvsPartition::<esp_idf_svc::nvs::NvsDefault>::take()?;
        let esp_wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;
        Ok(Self {
            wifi: Arc::new(Mutex::new(esp_wifi)),
            sysloop: sysloop,
        })
    }
}

impl WifiStation for Esp32WifiDriver {
    fn connect(&mut self, ssid: &str, password: &str) -> Result<()> {
        let mut auth_method = AuthMethod::WPA2Personal;
        if ssid.is_empty() {
            bail!("Missing WiFi name")
        }
        if password.is_empty() {
            auth_method = AuthMethod::None;
            info!("Wifi password is empty");
        }

        // let mut esp_wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;
        let mut esp_wifi = self.wifi.lock().unwrap();

        let mut wifi = BlockingWifi::wrap(&mut *esp_wifi, self.sysloop.clone())?;

        wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;

        info!("Starting wifi...");

        wifi.start()?;

        info!("Scanning...");

        let ap_infos = wifi.scan()?;

        let ours = ap_infos.into_iter().find(|a| a.ssid == ssid);

        let channel = if let Some(ours) = ours {
            info!(
                "Found configured access point {} on channel {}",
                ssid, ours.channel
            );
            Some(ours.channel)
        } else {
            info!(
            "Configured access point {} not found during scanning, will go with unknown channel",
            ssid
        );
            None
        };

        wifi.set_configuration(&Configuration::Client(ClientConfiguration {
            ssid: ssid
                .try_into()
                .expect("Could not parse the given SSID into WiFi config"),
            password: password
                .try_into()
                .expect("Could not parse the given password into WiFi config"),
            channel,
            auth_method,
            ..Default::default()
        }))?;

        info!("Connecting wifi...");

        wifi.connect()?;

        info!("Waiting for DHCP lease...");

        wifi.wait_netif_up()?;

        let ip_info = wifi.wifi().sta_netif().get_ip_info()?;

        info!("Wifi DHCP info: {:?}", ip_info);
        Ok(())
    }

    fn disconnect(&mut self) -> Result<()> {
        todo!()
    }

    fn is_connected(&self) -> Result<bool> {
        todo!()
    }

    fn get_mac_address(&self) -> Result<String> {
        todo!()
    }

    fn get_ip_address(&self) -> Result<String> {
        todo!()
    }
}

pub fn wifi(
    ssid: &str,
    pass: &str,
    modem: impl peripheral::Peripheral<P = esp_idf_svc::hal::modem::Modem> + 'static,
    sysloop: EspSystemEventLoop,
) -> Result<Box<EspWifi<'static>>> {
    let mut auth_method = AuthMethod::WPA2Personal;
    if ssid.is_empty() {
        bail!("Missing WiFi name")
    }
    if pass.is_empty() {
        auth_method = AuthMethod::None;
        info!("Wifi password is empty");
    }
    let nvs = EspNvsPartition::<esp_idf_svc::nvs::NvsDefault>::take()?;
    let mut esp_wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;

    let mut wifi = BlockingWifi::wrap(&mut esp_wifi, sysloop)?;

    wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;

    info!("Starting wifi...");

    wifi.start()?;

    info!("Scanning...");

    let ap_infos = wifi.scan()?;

    let ours = ap_infos.into_iter().find(|a| a.ssid == ssid);

    let channel = if let Some(ours) = ours {
        info!(
            "Found configured access point {} on channel {}",
            ssid, ours.channel
        );
        Some(ours.channel)
    } else {
        info!(
            "Configured access point {} not found during scanning, will go with unknown channel",
            ssid
        );
        None
    };

    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: ssid
            .try_into()
            .expect("Could not parse the given SSID into WiFi config"),
        password: pass
            .try_into()
            .expect("Could not parse the given password into WiFi config"),
        channel,
        auth_method,
        ..Default::default()
    }))?;

    info!("Connecting wifi...");

    wifi.connect()?;

    info!("Waiting for DHCP lease...");

    wifi.wait_netif_up()?;

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;

    info!("Wifi DHCP info: {:?}", ip_info);

    Ok(Box::new(esp_wifi))
}
