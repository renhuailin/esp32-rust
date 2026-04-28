use std::{
    collections::{HashSet, VecDeque},
    sync::{mpsc::Sender, Arc, Mutex, MutexGuard},
};

use anyhow::{Error, Ok, Result};
use esp_idf_hal::{
    gpio::{AnyInputPin, Pin},
    i2c::{I2cConfig, I2cDriver},
    i2s::{
        config::{DataBitWidth, StdConfig, TdmConfig},
        I2sBiDir, I2sDriver,
    },
    ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver},
    peripheral,
    prelude::{Peripherals, *},
    spi::SpiDriver,
};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use log::{error, info};

use crate::{
    audio::codec::{audio_codec::AudioCodec, xiaozhi_audio_codec::XiaozhiAudioCodec},
    axp173::{Axp173, Ldo},
    boards::board::Board,
    common::{application_context::ApplicationContext, gpio_button::Button},
    display::{lcd::st7789::LcdSt7789, Display},
    i2s::mixed_i2s::MixedI2sDriver,
    wifi::{
        ssid_manager::SsidMananger,
        wifi_driver::{Esp32WifiDriver, WifiAP, WifiStation},
    },
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
    wifi_config_mode: bool,
    app_context: ApplicationContext,
}

impl JiangLianS3CamBoard {
    pub fn new(app_context: ApplicationContext) -> Result<Self, Error> {
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

        let timer_driver = LedcTimerDriver::new(
            peripherals.ledc.timer0,
            &TimerConfig::new().frequency(25000.Hz().into()),
        )
        .unwrap();
        let backlight_pin = pins.gpio8;

        // 2. 配置LEDC通道，并绑定到背光引脚
        let channel_led: LedcDriver<'_> =
            LedcDriver::new(peripherals.ledc.channel0, timer_driver, backlight_pin).unwrap();

        let display = LcdSt7789::new(driver, dc.into(), cs.into(), channel_led)?;
        // display.init()?;

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

        // 初始化I2S
        // let tdm_config = TdmConfig::default();
        let std_config = StdConfig::philips(16000, DataBitWidth::Bits16);

        let bclk = pins.gpio42;
        let din = pins.gpio45;
        let dout = pins.gpio39;
        let mclk = pins.gpio41;
        let ws = pins.gpio40;

        // i2s_config
        let mut i2s_driver = I2sDriver::<I2sBiDir>::new_std_bidir(
            peripherals.i2s0,
            &std_config,
            bclk,
            din,
            dout,
            Some(mclk),
            ws,
        )
        .unwrap();

        // let i2s_driver = MixedI2sDriver::new(
        //     16000,
        //     mclk.pin(),
        //     bclk.pin(),
        //     ws.pin(),
        //     dout.pin(),
        //     din.pin(),
        //     4,
        // )
        // .unwrap();

        // i2s_driver.tx_enable().unwrap();
        // i2s_driver.rx_enable().unwrap();

        let audio_codec = XiaozhiAudioCodec::new(es8311_i2c_proxy, es7210_i2c_proxy, i2s_driver);

        Ok(Self {
            wifi_driver,
            display: Box::new(display),
            audio_codec: Arc::new(Mutex::new(audio_codec)),
            bus_manager,
            touch_button,
            volume_button,
            on_touch_button_clicked: None,
            on_volume_button_clicked: None,
            wifi_config_mode: false,
            app_context,
        })
    }

    pub fn init(&mut self) -> Result<()> {
        // self.init_wifi()?;
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
        info!("Power controller reg33: {:08b}", power_control_value);

        let reg12_value = axp173
            .read_u8(0x12)
            .map_err(|e| anyhow::anyhow!("Failed to read register 0x12: {:?}", e))?;

        info!("Power controller reg12: {:08b}", reg12_value);

        axp173
            .set_exten(true)
            .map_err(|e| anyhow::anyhow!("Failed to set EXTEN: {:?}", e))?;

        info!("Init power management done");
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

    fn wifi_scan(&mut self) -> Result<()> {
        let ssid = "CU_liu81802";
        let password = "china-ops";

        // let ssid = "1802";
        // let password = "20250101";

        self.wifi_driver.connect(ssid, password)?;
        Ok(())
    }

    // fn start_wifi_ap(&mut self) -> Result<()> {
    //     let ssid = "xiaozhi_ap";
    //     let password = "";

    //     self.wifi_driver.start_ap(ssid, password)?;

    //     Ok(())
    // }
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
        self.wifi_scan()?;
        // let wifi_connected = self.start_wifi_station()?;
        // if !wifi_connected {
        //     self.start_wifi_ap()?;
        // }

        Ok(())
    }

    fn get_wifi_driver(&self) -> &Self::WifiDriver {
        &self.wifi_driver
    }

    fn get_audio_codec(&mut self) -> Arc<Mutex<dyn AudioCodec>> {
        return Arc::clone(&self.audio_codec);
    }

    fn start_wifi_station(&mut self) -> std::result::Result<bool, Error> {
        let ssid_manager = SsidMananger::get_instance();

        // scanning available access points
        let available_ap_names = self.wifi_driver.get_available_access_points()?;
        info!("Available AP names: {:?}", available_ap_names);

        // get saved ssid list
        let ssid_list = ssid_manager.get_ssid_list()?;
        info!("Saved SSID list: {:?}", ssid_list);

        let saved_ap_names = ssid_list
            .iter()
            .map(|ssid| ssid.ssid.clone())
            .collect::<Vec<_>>();

        let set1: HashSet<_> = available_ap_names.iter().collect();
        let set2: HashSet<_> = saved_ap_names.iter().collect();

        // 2. 使用 intersection 找交集
        // 返回的是一个迭代器，里面依然是引用 (&&i32)
        let intersection: Vec<&String> = set1
            .intersection(&set2)
            .copied() // 把 &&i32 解引用成 i32 (也就是克隆一层引用指向的值)
            .collect();

        info!("Intersection: {:?}", intersection);

        for ssid_item in ssid_list {
            if intersection.contains(&&ssid_item.ssid) {
                let connet_result = self
                    .wifi_driver
                    .connect(ssid_item.ssid.as_str(), ssid_item.password.as_str());

                if let Err(_) = connet_result {
                    continue;
                } else {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    fn start_wifi_ap(&mut self) -> std::result::Result<bool, Error> {
        match self.wifi_driver.start_ap("xiaozhi_ap", "") {
            std::result::Result::Ok(ip_info) => match self.wifi_driver.start_http_server() {
                std::result::Result::Ok(_) => {
                    info!(
                        "成功启动http server,请访问 http://{}",
                        ip_info.ip.to_string()
                    );

                    let url = format!("http://{}", ip_info.ip.to_string());

                    self.display.show_qrcode(&url);
                    self.app_context.app_event_sender.send(
                        crate::common::event::AppEvent::PlayAudioAlert("wificonfig".to_string()),
                    )?;
                }
                Err(e) => {
                    error!("启动http server 出错：{:?}", e);
                    return Err(e.into());
                }
            },
            Err(err) => {
                error!("启动http server 出错：{:?}", err);
            }
        }

        Ok(true)
    }

    fn start_network(&mut self) -> Result<()> {
        self.wifi_scan()?;

        // info!("Start network");
        // let wifi_connected = self.start_wifi_station()?;
        // if !wifi_connected {
        //     self.wifi_config_mode = true;
        //     self.start_wifi_ap()?;
        // }

        Ok(())
    }
}
