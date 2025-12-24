use std::{
    collections::VecDeque,
    sync::{mpsc::Sender, MutexGuard},
};

use anyhow::{Error, Ok, Result};
use esp_idf_hal::{
    i2c::I2cDriver,
    i2s::{I2sBiDir, I2sDriver},
    peripheral,
    prelude::Peripherals,
};
use esp_idf_svc::{eventloop::EspSystemEventLoop, wifi::WifiDeviceId};
use log::info;

use crate::{
    axp173::{Axp173, Ldo},
    boards::board::Board,
    common::event::XzEvent,
    protocols::websocket::ws_protocol::WebSocketProtocol,
    wifi::{self, Esp32WifiDriver, WifiStation},
};
use shared_bus::{BusManager, BusManagerSimple};

pub struct JiangLianS3CamBoard {
    wifi_driver: Esp32WifiDriver,
}

impl JiangLianS3CamBoard {
    pub fn new() -> Result<Self, Error> {
        let peripherals: Peripherals = Peripherals::take().unwrap();
        let sysloop = EspSystemEventLoop::take()?;
        let wifi_driver = Esp32WifiDriver::new(peripherals.modem, sysloop.clone())?;
        Ok(Self { wifi_driver })
    }

    pub fn init(&mut self) -> Result<()> {
        self.init_wifi()?;
        Ok(())
    }

    fn init_power_management(
        &mut self,
        bus_manager: &BusManager<shared_bus::NullMutex<I2cDriver<'_>>>,
    ) {
        let axp173_i2c_proxy = bus_manager.acquire_i2c();
        // 2. 创建AXP173驱动实例
        let mut axp173 = Axp173::new(axp173_i2c_proxy);
        axp173.init().unwrap();

        // 根据axp173手册，LDO4的电压由一个byte,8位bit表示，电压范围是：0.7-3.5V， 25mV/step，每个bit表示25mV。
        // 所以要设置LDO4的电压为3.3V  (3300 - 700) / 25 = 104
        let ldo4 = Ldo::ldo4_with_voltage(104, true);

        // 根据axp173手册，LDO2,LDO3的电压由一个byte,低4位bit表示LDO3的电压，高4位表示LDO2的电压，电压范围是：1.8-3.3V， 100mV/step，每个bit表示100mV。
        // 所以要设置LDO2,LDO3的电压为2.8V  (2800 - 1800) / 100 = 10
        let ldo2 = Ldo::ldo2_with_voltage(10, true);
        axp173.enable_ldo(&ldo2).unwrap();
        axp173.enable_ldo(&ldo4).unwrap();

        let power_control_value = axp173.read_u8(0x33).unwrap();
        println!("Power controller reg33: {:08b}", power_control_value);

        let reg12_value = axp173.read_u8(0x12).unwrap();
        println!("Power controller reg12: {:08b}", reg12_value);

        axp173.set_exten(true).unwrap();
    }
}

impl Board for JiangLianS3CamBoard {
    type WifiDriver = Esp32WifiDriver;

    fn init_wifi(&mut self) -> std::result::Result<(), Error> {
        let ssid = "CU_liu81802";
        let password = "china-ops";

        self.wifi_driver.connect(ssid, password)?;
        Ok(())
    }
}
