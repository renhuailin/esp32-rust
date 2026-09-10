use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use anyhow::{Error, Ok, Result};
use chrono::Utc;
use esp_idf_hal::{
    gpio::{AnyInputPin, PinDriver},
    i2c::{I2cConfig, I2cDriver},
    i2s::{
        config::{
            Config, DataBitWidth, SlotMode, StdClkConfig, StdConfig, StdGpioConfig, StdSlotConfig,
        },
        I2sBiDir, I2sDriver,
    },
    ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver},
    peripherals::Peripherals,
    spi::SpiDriver,
    units::*,
};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use log::{error, info};

use crate::{
    audio::codec::{audio_codec::AudioCodec, xiaozhi_audio_codec::XiaozhiAudioCodec},
    axp173::Axp173,
    boards::board::Board,
    common::{application_context::ApplicationContext, event::AppEvent, gpio_button::Button},
    display::{lcd::st7789::LcdSt7789, BatteryStatus, Display},
    wifi::{
        ssid_manager::SsidMananger,
        wifi_driver::{Esp32WifiDriver, WifiAP, WifiStation},
    },
};
use shared_bus::{BusManager, BusManagerStd, I2cProxy};

pub struct JiangLianS3CamBoard {
    wifi_driver: Esp32WifiDriver,
    pub display: LcdSt7789,
    audio_codec: Arc<Mutex<dyn AudioCodec + 'static>>,
    bus_manager: &'static BusManager<Mutex<I2cDriver<'static>>>,
    /// AXP173 电源管理实例（init_power_management 后可用），
    /// 提供电量/充电/USB 状态读取。
    power_manager: Option<Axp173<I2cProxy<'static, Mutex<I2cDriver<'static>>>>>,

    speak_button: &'static mut Button,
    volume_button: &'static mut Button,

    on_speak_button_clicked: Option<Box<dyn FnMut() + Send + 'static>>,
    on_volume_button_clicked: Option<Box<dyn FnMut() + Send + 'static>>,
    on_volume_long_pressed: Option<Box<dyn FnMut() + Send + 'static>>,
    on_wifi_connected: Option<Box<dyn FnMut(String, String) + Send + 'static>>, // wifi 连接后回调

    wifi_config_mode: bool,
    app_context: ApplicationContext,
    /// NS4830 功放使能脚：拉高后由结构体长期持有，
    /// 确保程序整个生命周期内引脚配置不被改写、电平保持高
    _ns4830_ctrl: PinDriver<'static, esp_idf_hal::gpio::Output>,
}

impl JiangLianS3CamBoard {
    pub fn new(app_context: ApplicationContext) -> Result<Self, Error> {
        let peripherals: Peripherals = Peripherals::take().unwrap();
        let pins = peripherals.pins;

        let sysloop = EspSystemEventLoop::take()?;
        let wifi_driver = Esp32WifiDriver::new(peripherals.modem, sysloop.clone())?;

        // NS4830 功放控制脚：用 PinDriver 包装成推挽输出再拉高，
        // 之后存入结构体长期持有，保证整个运行期间引脚一直输出高电平
        // （pins.gpio10 只是引脚令牌，本身没有 set_high 方法）
        let mut ns4830_ctrl = PinDriver::output(pins.gpio10)?;
        ns4830_ctrl.set_high()?;

        // GPIO21 上升沿监控（硬件中断方式）：输入 + 下拉（空闲为低），
        // 注册 GPIO 正边沿中断；等待线程空闲时完全休眠（零 CPU 占用），
        // 中断触发被唤醒后向应用主循环投递 AppEvent::Gpio21RisingEdge。
        // PinDriver 泄漏成 'static 供监控线程独占持有。
        let gpio21 = PinDriver::input(pins.gpio21, esp_idf_hal::gpio::Pull::Down)?;
        let gpio21 = Box::leak(Box::new(gpio21));
        let gpio21_sender = app_context.app_event_sender.clone();
        let _ = std::thread::Builder::new()
            .name("gpio21_monitor".into())
            .stack_size(4 * 1024)
            .spawn(move || loop {
                // 阻塞等待上升沿：内部挂 GPIO ISR，事件驱动，无轮询
                if let Err(e) = esp_idf_hal::task::block_on(gpio21.wait_for_rising_edge()) {
                    log::error!("GPIO21 wait_for_rising_edge failed: {:?}", e);
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                    continue;
                }
                log::info!("GPIO21 rising edge detected");
                if let Err(e) = gpio21_sender.send(AppEvent::Gpio21RisingEdge) {
                    log::error!("Failed to send Gpio21RisingEdge: {:?}", e);
                    break;
                }
            });

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
        // display.show_qrcode("fdsfsdfds");

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
        // 注意 1：DMA 描述符数量不能动（dma_buffer_count(16) 实测导致拾音失效），
        // 播放卡顿用软件 jitter buffer 解决（见 application.rs）。
        // 注意 2：auto_clear(true) 必须保留——TX DMA underrun 时自动发静音；
        // 为 false 时 DMA 会循环重发最后一个缓冲的残留音频（~32ms 片段），
        // 网络抖动导致的短暂断流会变成"嗒嗒嗒"重复声/爆音。
        // 组装方式 = philips() 的等价展开（esp-idf-hal 0.46.x StdConfig 字段私有），
        // 仅 channel Config 加 auto_clear(true)，DMA 深度保持默认 6。
        let std_config = StdConfig::new(
            Config::default().auto_clear(true),
            StdClkConfig::from_sample_rate_hz(16000),
            StdSlotConfig::philips_slot_default(DataBitWidth::Bits16, SlotMode::Stereo),
            StdGpioConfig::default(),
        );

        let bclk = pins.gpio42;
        let din = pins.gpio45;
        let dout = pins.gpio39;
        let mclk = pins.gpio41;
        let ws = pins.gpio40;

        // i2s_config
        let i2s_driver = I2sDriver::<I2sBiDir>::new_std_bidir(
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
            display: display,
            audio_codec: Arc::new(Mutex::new(audio_codec)),
            bus_manager,
            power_manager: None,
            speak_button: touch_button,
            volume_button,
            _ns4830_ctrl: ns4830_ctrl,
            on_speak_button_clicked: None,
            on_volume_button_clicked: None,
            wifi_config_mode: false,
            app_context,
            on_volume_long_pressed: None,
            on_wifi_connected: None,
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

        // // 根据axp173手册，LDO4的电压由一个byte,8位bit表示，电压范围是：0.7-3.5V， 25mV/step，每个bit表示25mV。
        // // 所以要设置LDO4的电压为3.3V  (3300 - 700) / 25 = 104
        // let ldo4 = Ldo::ldo4_with_voltage(104, true);

        // // 根据axp173手册，LDO2,LDO3的电压由一个byte,低4位bit表示LDO3的电压，高4位表示LDO2的电压，电压范围是：1.8-3.3V， 100mV/step，每个bit表示100mV。
        // // 所以要设置LDO2,LDO3的电压为2.8V  (2800 - 1800) / 100 = 10
        // let ldo2 = Ldo::ldo2_with_voltage(10, true);
        // axp173
        //     .enable_ldo(&ldo2)
        //     .map_err(|e| anyhow::anyhow!("Failed to enable LDO2: {:?}", e))?;
        // axp173
        //     .enable_ldo(&ldo4)
        //     .map_err(|e| anyhow::anyhow!("Failed to enable LDO4: {:?}", e))?;

        // let power_control_value = axp173
        //     .read_u8(0x33)
        //     .map_err(|e| anyhow::anyhow!("Failed to read register 0x33: {:?}", e))?;
        // info!("Power controller reg33: {:08b}", power_control_value);

        // let reg12_value = axp173
        //     .read_u8(0x12)
        //     .map_err(|e| anyhow::anyhow!("Failed to read register 0x12: {:?}", e))?;

        // info!("Power controller reg12: {:08b}", reg12_value);

        axp173
            .set_exten(true)
            .map_err(|e| anyhow::anyhow!("Failed to set EXTEN: {:?}", e))?;

        // 持有实例，供运行期读取电池/充电/USB 状态
        self.power_manager = Some(axp173);

        info!("Init power management done");
        Ok(())
    }

    /// 从 AXP173 读取电池状态（电量/充电/放电/USB 在位）。
    /// 供状态栏定时刷新（AppEvent::RefreshBattery -> Display::show_battery_status）。
    fn read_battery_status_impl(&mut self) -> Result<BatteryStatus> {
        let pm = self
            .power_manager
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("AXP173 not initialized"))?;

        let level = pm
            .battery_level()
            .map_err(|e| anyhow::anyhow!("read battery level: {:?}", e))?;
        let charging = pm
            .battery_charging()
            .map_err(|e| anyhow::anyhow!("read charging: {:?}", e))?;
        let charge_done = pm
            .is_charge_done()
            .map_err(|e| anyhow::anyhow!("read charge done: {:?}", e))?;
        let discharging = pm
            .is_discharging()
            .map_err(|e| anyhow::anyhow!("read discharging: {:?}", e))?;
        let vbus_present = pm
            .vbus_present()
            .map_err(|e| anyhow::anyhow!("read vbus: {:?}", e))?;

        Ok(BatteryStatus {
            level,
            charging,
            charge_done,
            discharging,
            vbus_present,
        })
    }

    fn init_buttons(&mut self) -> Result<()> {
        println!("Init buttons");
        if let Some(on_clicked) = self.on_speak_button_clicked.take() {
            self.speak_button.on_click(on_clicked)?;
        }

        if let Some(on_clicked) = self.on_volume_button_clicked.take() {
            self.volume_button.on_click(on_clicked)?;
        }

        if let Some(long_pressed_callback) = self.on_volume_long_pressed.take() {
            self.volume_button.on_long_press(long_pressed_callback)?;
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

    fn on_speak_button_clicked(&mut self, on_clicked: Box<dyn FnMut() + Send + 'static>) {
        self.on_speak_button_clicked = Some(on_clicked);
    }

    fn on_volume_button_clicked(&mut self, on_clicked: Box<dyn FnMut() + Send + 'static>) {
        self.on_volume_button_clicked = Some(on_clicked);
    }

    fn on_volume_button_long_pressed(&mut self, on_clicked: Box<dyn FnMut() + Send + 'static>) {
        self.on_volume_long_pressed = Some(on_clicked);
    }

    fn set_on_wifi_connected_callback(
        &mut self,
        on_connected: Box<dyn FnMut(String, String) + Send + 'static>,
    ) {
        self.on_wifi_connected = Some(on_connected);
    }

    fn init_wifi(&mut self) -> std::result::Result<(), Error> {
        // // self.wifi_scan()?;
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

    fn start_wifi_station(&mut self) -> std::result::Result<(bool, Vec<String>), Error> {
        let mut ssid_manager = SsidMananger::get_instance();

        // scanning available access points
        let available_ap_names = match self.wifi_driver.get_available_access_points() {
            std::result::Result::Ok(aps) => aps,
            Err(err) => {
                log::error!("Scan failed with EspError: {:?}", err);
                error!("Failed to get available access points: {}", err);
                vec![]
            }
        };
        info!("Available AP names: {:?}", available_ap_names);

        // get saved ssid list
        let saved_ssid_list = ssid_manager.get_saved_ssid_list()?;
        info!("Saved SSID list: {:?}", saved_ssid_list);

        let saved_ap_names = saved_ssid_list
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
        if intersection.is_empty() {
            for mut ssid_item in saved_ssid_list {
                let connet_result = self
                    .wifi_driver
                    .connect(ssid_item.ssid.as_str(), ssid_item.password.as_str());

                if let Err(_) = connet_result {
                    continue;
                } else {
                    info!("Connected to saved ssid: {}", ssid_item.ssid);
                    //更新这个ssid的最后连接时间
                    ssid_item.last_connect_time = Utc::now().to_rfc3339();
                    ssid_manager.update_ssid_item(ssid_item)?;
                    return Ok((true, available_ap_names));
                }
            }
        } else {
            for mut ssid_item in saved_ssid_list {
                if intersection.contains(&&ssid_item.ssid) {
                    let connet_result = self
                        .wifi_driver
                        .connect(ssid_item.ssid.as_str(), ssid_item.password.as_str());

                    if let Err(_) = connet_result {
                        continue;
                    } else {
                        info!("Connected to saved ssid: {}", ssid_item.ssid);
                        //更新这个ssid的最后连接时间
                        let last_connect_time = Utc::now().to_rfc3339();
                        info!("current time: {}", last_connect_time);
                        ssid_item.last_connect_time = last_connect_time;
                        let ssid = ssid_item.ssid.clone();

                        ssid_manager.update_ssid_item(ssid_item)?;

                        let mac_address = self.wifi_driver.get_mac_address()?;
                        info!("mac_address: {}", mac_address);

                        if let Some(mut on_wifi_connectd) = self.on_wifi_connected.take() {
                            on_wifi_connectd(ssid, mac_address.to_string());
                        }

                        return Ok((true, available_ap_names));
                    }
                }
            }
        }

        Ok((false, available_ap_names))
    }

    fn start_wifi_ap(&mut self, ap_names: Vec<String>) -> std::result::Result<bool, Error> {
        match self.wifi_driver.start_ap("xiaozhi_ap", "") {
            std::result::Result::Ok(ip_info) => {
                match self.wifi_driver.start_http_server(ap_names) {
                    std::result::Result::Ok(_) => {
                        info!(
                            "成功启动http server,请访问 http://{}",
                            ip_info.ip.to_string()
                        );

                        let url = format!("http://{}", ip_info.ip.to_string());

                        self.display.show_qrcode(&url);
                        self.app_context.app_event_sender.send(
                            crate::common::event::AppEvent::PlayAudioAlert(
                                "wificonfig".to_string(),
                            ),
                        )?;
                    }
                    Err(e) => {
                        error!("启动http server 出错：{:?}", e);
                        return Err(e.into());
                    }
                }
            }
            Err(err) => {
                error!("启动http server 出错：{:?}", err);
            }
        }

        Ok(true)
    }

    fn start_network(&mut self) -> Result<()> {
        info!("Start network");

        // self.wifi_scan()?;

        let (wifi_connected, ap_names) = self.start_wifi_station()?;
        if !wifi_connected {
            self.wifi_config_mode = true;
            self.start_wifi_ap(ap_names)?;
        }

        Ok(())
    }

    type DisplayDriver = LcdSt7789;

    fn get_display(&mut self) -> &mut Self::DisplayDriver {
        return &mut self.display;
    }

    fn read_battery_status(&mut self) -> Result<BatteryStatus> {
        self.read_battery_status_impl()
    }
}
