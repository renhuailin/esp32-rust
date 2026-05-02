use std::sync::{Arc, Mutex};

use anyhow::{bail, Error, Ok, Result};
use esp_idf_hal::modem::WifiModemPeripheral;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    http::server::EspHttpServer,
    ipv4::IpInfo,
    nvs::{EspCustomNvsPartition, EspNvsPartition, NvsCustom},
    wifi::{
        AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration,
        EspWifi, WifiDeviceId,
    },
};
use log::{error, info};

use crate::common::httpd_server::{create_server, start_http_server};

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

    /// 获取可用的WIFI接入点列表
    fn get_available_access_points(&self) -> Result<Vec<String>>;

    // 如果你需要省电管理，可以加这个
    // fn set_power_save(&mut self, enabled: bool) -> Result<()>;

    fn set_on_new_access_point_add_handler(
        &mut self,
        on_new_access_point_add: Box<dyn FnMut(&str, &str) -> Result<(), Error> + Send + 'static>,
    );
}

pub trait WifiAP {
    fn start_ap(&mut self, ssid: &str, password: &str) -> Result<IpInfo>;
    fn start_http_server(&mut self) -> Result<()>;
    fn stop_http_server(&mut self) -> Result<()>;
}

pub struct Esp32WifiDriver {
    // 使用 Arc<Mutex<>> 是为了线程安全，因为 EspWifi 可能需要在多处共享
    // 或者你可以直接持有 &mut EspWifi，取决于你的架构
    wifi: Arc<Mutex<EspWifi<'static>>>,
    sysloop: EspSystemEventLoop,

    /// 当配置了一个新的wifi接入点(AP)时的回调函数,application需要用这个回调来重新配置网络并连接到这个ap上。
    on_new_access_point_add:
        Option<Box<dyn FnMut(&str, &str) -> Result<(), Error> + Send + 'static>>,

    http_server: Option<EspHttpServer<'static>>,
}

impl Esp32WifiDriver {
    pub fn new(
        modem: impl WifiModemPeripheral + 'static,
        sysloop: EspSystemEventLoop,
    ) -> Result<Self> {
        let esp_wifi = EspWifi::new(modem, sysloop.clone(), None)?;
        Ok(Self {
            wifi: Arc::new(Mutex::new(esp_wifi)),
            sysloop: sysloop,
            on_new_access_point_add: None,
            http_server: None,
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
        self.wifi.lock().unwrap().disconnect()?;
        Ok(())
    }

    fn is_connected(&self) -> Result<bool> {
        Ok(self.wifi.lock().unwrap().is_connected()?)
    }

    fn get_mac_address(&self) -> Result<String> {
        let mac_address_bytes = self.wifi.lock().unwrap().get_mac(WifiDeviceId::Sta)?;
        let mac_address_str = mac_address_bytes
            .iter()
            .map(|&b| format!("{:02X}", b)) // :02X 表示两位、大写的十六进制数，不足则补零
            .collect::<Vec<String>>()
            .join(":");
        Ok(mac_address_str)
    }

    fn get_ip_address(&self) -> Result<String> {
        todo!()
    }

    fn get_available_access_points(&self) -> Result<Vec<String>> {
        // let mut esp_wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;
        let mut esp_wifi = self.wifi.lock().unwrap();

        let mut wifi = BlockingWifi::wrap(&mut *esp_wifi, self.sysloop.clone())?;

        info!("Scanning...");

        wifi.start()?;
        let ap_infos = wifi.scan()?;
        let ap_names = ap_infos
            .into_iter()
            .map(|a| a.ssid.to_string())
            .collect::<Vec<String>>();
        Ok(ap_names)
    }

    fn set_on_new_access_point_add_handler(
        &mut self,
        on_new_access_point_add: Box<dyn FnMut(&str, &str) -> Result<(), Error> + Send + 'static>,
    ) {
        self.on_new_access_point_add = Some(on_new_access_point_add);
    }
}

impl WifiAP for Esp32WifiDriver {
    fn start_ap(&mut self, ssid: &str, password: &str) -> Result<IpInfo> {
        // 1. 初始化 EspWifi 驱动
        let mut wifi = self.wifi.lock().unwrap();

        // 2. 根据密码长度自动选择认证方式
        let auth_method = if password.is_empty() {
            info!("AP password is empty, setting to Open network.");
            AuthMethod::None
        } else if password.len() < 8 {
            bail!("WiFi password must be at least 8 characters long");
        } else {
            AuthMethod::WPA2Personal
        };

        // 3. 构建 AP 配置
        let ap_config = AccessPointConfiguration {
            ssid: ssid
                .try_into()
                .expect("Could not parse the given SSID into WiFi config"),
            password: password
                .try_into()
                .expect("Could not parse the given pasword into WiFi config"),
            auth_method,
            // 其他可选的高级配置：
            // ssid_hidden: false,       // 是否隐藏 SSID
            // max_connections: 4,       // 最大连接数 (ESP32 最大通常是 10)
            // channel: 1,               // WiFi 信道 (1-13)
            ..Default::default()
        };

        // 4. 将配置包裹在 Configuration::AccessPoint 枚举中
        let config = Configuration::AccessPoint(ap_config);
        wifi.set_configuration(&config)?;

        // 5. 启动 WiFi (此时 WiFi 硬件开始工作)
        info!("Starting WiFi in AP mode...");
        wifi.start()?;

        // 6. AP 模式不需要 connect()，直接就是 Up 状态
        // 但为了确保系统网络栈（LwIP）准备就绪，我们可以检查一下
        // if !wifi.is_up()? {
        //     bail!("Failed to bring up WiFi AP");
        // }

        // 7. 获取并打印 ESP32 分配到的网关 IP 地址
        // 默认情况下，ESP32 作为一个 DHCP 服务器，它的 IP 通常是 192.168.4.1
        let netif = wifi.ap_netif();
        let ip_info = netif.get_ip_info()?;

        info!("AP Started Successfully!");
        info!("SSID: {}", ssid);
        info!("IP Address: {}", ip_info.ip);
        info!("Subnet Mask: {}", ip_info.subnet.mask);

        Ok(ip_info)
    }

    fn start_http_server(&mut self) -> Result<()> {
        // let server_configuration = esp_idf_svc::http::server::Configuration {
        //     stack_size: STACK_SIZE,
        //     ..Default::default()
        // };

        let mut http_server = create_server()?;
        start_http_server(&mut http_server)?;

        // let on_new_access_point_add = &self.on_new_access_point_add;

        self.http_server = Some(http_server);
        Ok(())
    }

    fn stop_http_server(&mut self) -> Result<()> {
        if let Some(http_server) = self.http_server.take() {
            drop(http_server);
        }
        Ok(())
    }
}

pub fn wifi(
    ssid: &str,
    pass: &str,
    modem: impl WifiModemPeripheral + 'static,
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

/// 配置并启动 WiFi AP 模式
///
/// `modem`: 外设的所有权 (Peripherals::take().unwrap().modem)
/// `sysloop`: 系统事件循环
/// `ssid`: 热点名称
/// `password`: 热点密码 (如果为空，则为开放网络)
pub fn start_ap<'a>(
    modem: impl WifiModemPeripheral + 'a,
    sysloop: EspSystemEventLoop,
    ssid: &str,
    password: &str,
) -> Result<EspWifi<'a>> {
    // 1. 初始化 EspWifi 驱动
    let mut wifi = EspWifi::new(modem, sysloop.clone(), None)?;

    // 2. 根据密码长度自动选择认证方式
    let auth_method = if password.is_empty() {
        info!("AP password is empty, setting to Open network.");
        AuthMethod::None
    } else if password.len() < 8 {
        bail!("WiFi password must be at least 8 characters long");
    } else {
        AuthMethod::WPA2Personal
    };

    // 3. 构建 AP 配置
    let ap_config = AccessPointConfiguration {
        ssid: ssid
            .try_into()
            .expect("Could not parse the given SSID into WiFi config"),
        password: password
            .try_into()
            .expect("Could not parse the given pasword into WiFi config"),
        auth_method,
        // 其他可选的高级配置：
        // ssid_hidden: false,       // 是否隐藏 SSID
        // max_connections: 4,       // 最大连接数 (ESP32 最大通常是 10)
        // channel: 1,               // WiFi 信道 (1-13)
        ..Default::default()
    };

    // 4. 将配置包裹在 Configuration::AccessPoint 枚举中
    let config = Configuration::AccessPoint(ap_config);
    wifi.set_configuration(&config)?;

    // 5. 启动 WiFi (此时 WiFi 硬件开始工作)
    info!("Starting WiFi in AP mode...");
    wifi.start()?;

    // 6. AP 模式不需要 connect()，直接就是 Up 状态
    // 但为了确保系统网络栈（LwIP）准备就绪，我们可以检查一下
    if !wifi.is_up()? {
        bail!("Failed to bring up WiFi AP");
    }

    // 7. 获取并打印 ESP32 分配到的网关 IP 地址
    // 默认情况下，ESP32 作为一个 DHCP 服务器，它的 IP 通常是 192.168.4.1
    let netif = wifi.ap_netif();
    let ip_info = netif.get_ip_info()?;

    info!("AP Started Successfully!");
    info!("SSID: {}", ssid);
    info!("IP Address: {}", ip_info.ip);
    info!("Subnet Mask: {}", ip_info.subnet.mask);

    Ok(wifi)
}
