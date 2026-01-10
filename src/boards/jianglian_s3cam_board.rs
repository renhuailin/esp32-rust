use std::{
    collections::VecDeque,
    sync::{mpsc::Sender, Arc, Mutex, MutexGuard},
};

use anyhow::{Error, Ok, Result};
use esp_idf_hal::{
    gpio::AnyInputPin,
    i2c::{I2cConfig, I2cDriver},
    i2s::{I2sBiDir, I2sDriver},
    peripheral,
    prelude::Peripherals,
    spi::SpiDriver,
};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use log::info;

use crate::{
    audio::codec::{audio_codec::AudioCodec, xiaozhi_audio_codec::XiaozhiAudioCodec},
    axp173::{Axp173, Ldo},
    boards::board::Board,
    common::{event::XzEvent, gpio_button::Button},
    display::{lcd::st7789::LcdSt7789, Display},
    protocols::websocket::ws_protocol::WebSocketProtocol,
    wifi::{self, Esp32WifiDriver, WifiStation},
};
use shared_bus::{BusManager, BusManagerStd};

pub struct JiangLianS3CamBoard {
    wifi_driver: Esp32WifiDriver,
    display: Box<dyn Display>,
    audio_codec: Arc<Mutex<dyn AudioCodec + 'static>>,
    bus_manager: &'static BusManager<Mutex<I2cDriver<'static>>>,
    touch_button: &'static mut Button,
    volume_button: &'static mut Button,

    on_touch_button_clicked: Option<Box<dyn FnMut() + Send + 'static>>,
    on_volume_button_clicked: Option<Box<dyn FnMut() + Send + 'static>>,
}

impl JiangLianS3CamBoard {
    pub fn new() -> Result<Self, Error> {
        let peripherals: Peripherals = Peripherals::take().unwrap();
        let pins = peripherals.pins;

        let sysloop = EspSystemEventLoop::take()?;
        let wifi_driver = Esp32WifiDriver::new(peripherals.modem, sysloop.clone())?;

        let dc = pins.gpio7;
        // SPI 总线引脚 (使用硬件 SPI2)
        let sck = pins.gpio5;
        let sdi = pins.gpio4; // MOSI 在驱动中通常被称为 SDI (Serial Data In)
        let sdo = Option::<AnyInputPin>::None; // MISO
        let cs = pins.gpio6; // 直接使用引脚，而不是PinDriver

        // 3. 初始化 SPI 驱动
        // 创建 SPI 驱动程序实例
        let spi3 = peripherals.spi3;
        let driver = SpiDriver::new(
            spi3, // 使用 SPI3
            sck,
            sdi,
            sdo,
            &Default::default(),
        )
        .unwrap();

        let display = LcdSt7789::new(driver, dc.into(), cs.into())?;

        // 初始化 I2C 驱动和总线管理器
        let sda = pins.gpio1;
        let scl = pins.gpio2;
        let i2c: esp_idf_hal::i2c::I2C1 = peripherals.i2c1;
        let config = I2cConfig::new();

        let i2c_driver = I2cDriver::new(i2c, sda, scl, &config).unwrap();

        // let bus_manager: shared_bus::BusManager<Mutex<I2cDriver<'_>>> =
        //     shared_bus::BusManager::new(i2c_driver);

        // let manager_box = Box::new(BusManagerSimple::new(i2c_driver));
        let manager_box = Box::new(BusManagerStd::new(i2c_driver));
        // let manager_box = Box::new(BusManagerSimple::new(i2c_driver));
        // let manager_box = Box::new(BusManager::new(i2c_driver));

        // 2. 使用 Box::leak()。
        //    这会消耗掉 Box，返回一个 &'static mut BusManager... 引用。
        //    这块内存将永远不会被释放（直到断电），从而满足了生命周期要求。
        let bus_manager = Box::leak(manager_box);

        let touch_button_box = Box::new(Button::new(0)?);
        let volume_button_box = Box::new(Button::new(47)?);
        let touch_button = Box::leak(touch_button_box);
        let volume_button = Box::leak(volume_button_box);

        // 现在从 bus_manager 获取 I2C 代理来创建 audio_codec
        let es8311_i2c_proxy = bus_manager.acquire_i2c();
        let es7210_i2c_proxy = bus_manager.acquire_i2c();

        let audio_codec = XiaozhiAudioCodec::new(es8311_i2c_proxy, es7210_i2c_proxy);

        Ok(Self {
            wifi_driver,
            display: Box::new(display),
            audio_codec: Arc::new(Mutex::new(audio_codec)),
            bus_manager,
            touch_button,
            volume_button,
            on_touch_button_clicked: None,
            on_volume_button_clicked: None,
        })
    }

    pub fn init(&mut self) -> Result<()> {
        self.init_wifi()?;
        info!("Init power management");
        self.init_power_management()?;
        info!("Init buttons");
        self.init_buttons()?;
        Ok(())
    }

    fn init_power_management(&mut self) -> Result<()> {
        let axp173_i2c_proxy = self.bus_manager.acquire_i2c();
        // 2. 创建AXP173驱动实例
        let mut axp173 = Axp173::new(axp173_i2c_proxy);
        axp173
            .init()
            .map_err(|e| anyhow::anyhow!("Failed to init AXP173: {:?}", e))?;

        // 根据axp173手册，LDO4的电压由一个byte,8位bit表示，电压范围是：0.7-3.5V， 25mV/step，每个bit表示25mV。
        // 所以要设置LDO4的电压为3.3V  (3300 - 700) / 25 = 104
        let ldo4 = Ldo::ldo4_with_voltage(104, true);

        // 根据axp173手册，LDO2,LDO3的电压由一个byte,低4位bit表示LDO3的电压，高4位表示LDO2的电压，电压范围是：1.8-3.3V， 100mV/step，每个bit表示100mV。
        // 所以要设置LDO2,LDO3的电压为2.8V  (2800 - 1800) / 100 = 10
        let ldo2 = Ldo::ldo2_with_voltage(10, true);
        axp173
            .enable_ldo(&ldo2)
            .map_err(|e| anyhow::anyhow!("Failed to enable LDO2: {:?}", e))?;
        axp173
            .enable_ldo(&ldo4)
            .map_err(|e| anyhow::anyhow!("Failed to enable LDO4: {:?}", e))?;

        let power_control_value = axp173
            .read_u8(0x33)
            .map_err(|e| anyhow::anyhow!("Failed to read register 0x33: {:?}", e))?;
        println!("Power controller reg33: {:08b}", power_control_value);

        let reg12_value = axp173
            .read_u8(0x12)
            .map_err(|e| anyhow::anyhow!("Failed to read register 0x12: {:?}", e))?;
        info!("Init power management done1111");
        println!("Power controller reg12: {:08b}", reg12_value);

        axp173
            .set_exten(true)
            .map_err(|e| anyhow::anyhow!("Failed to set EXTEN: {:?}", e))?;
        println!("Init power management done");
        Ok(())
    }

    fn init_buttons(&mut self) -> Result<()> {
        println!("Init buttons");
        if let Some(on_clicked) = self.on_touch_button_clicked.take() {
            self.touch_button.on_click(on_clicked)?;
        }

        if let Some(on_clicked) = self.on_volume_button_clicked.take() {
            self.volume_button.on_click(on_clicked)?;
        }

        Ok(())
    }
}

impl Board for JiangLianS3CamBoard {
    type WifiDriver = Esp32WifiDriver;

    fn on_touch_button_clicked(&mut self, on_clicked: Box<dyn FnMut() + Send + 'static>) {
        self.on_touch_button_clicked = Some(on_clicked);
    }

    fn on_volume_button_clicked(&mut self, on_clicked: Box<dyn FnMut() + Send + 'static>) {
        self.on_volume_button_clicked = Some(on_clicked);
    }

    fn init_wifi(&mut self) -> std::result::Result<(), Error> {
        let ssid = "CU_liu81802";
        let password = "china-ops";

        self.wifi_driver.connect(ssid, password)?;
        Ok(())
    }

    fn get_wifi_driver(&self) -> &Self::WifiDriver {
        &self.wifi_driver
    }

    fn get_audio_codec(&mut self) -> Arc<Mutex<dyn AudioCodec>> {
        return Arc::clone(&self.audio_codec);
    }
}
