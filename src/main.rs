use esp_idf_hal::{
    delay::{Delay, FreeRtos, BLOCK},
    gpio::{self, AnyIOPin, AnyInputPin, AnyOutputPin, PinDriver},
    i2c::{I2cConfig, I2cDriver},
    i2s::{
        config::{DataBitWidth, StdConfig},
        I2sBiDir, I2sDriver,
    },
    ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver},
    prelude::*,
    rmt::RmtChannel,
    spi::SpiDriver,
};

use esp_idf_hal::delay;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::prelude::*;
use esp_idf_sys::EspError;
use esp_idf_test2::{
    audio,
    axp173::{Axp173, Ldo},
    lcd,
    led::WS2812RMT,
    wifi::wifi,
};
use log::info;
use mipidsi::error;
use shared_bus::BusManagerSimple;

fn main() {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();
    let peripherals: Peripherals = Peripherals::take().unwrap();

    // let app_config = CONFIG;
    let pins = peripherals.pins;
    let spi3 = peripherals.spi3;

    // 1. 初始化I2C总线。
    //    !!! 警告: 您必须根据开发板的原理图，确认AXP173连接的是哪个I2C总线和引脚！
    let sda = pins.gpio1;
    let scl = pins.gpio2;
    let i2c = peripherals.i2c1;
    let config = I2cConfig::new();

    let i2c_driver = I2cDriver::new(i2c, sda, scl, &config).unwrap();

    // 2. 创建一个总线管理器，并将I2C驱动的所有权交给它
    let bus_manager = BusManagerSimple::new(i2c_driver);

    // 3. 从管理器中为每个设备创建独立的“代理”
    //    axp_i2c_proxy 和 es_i2c_proxy 现在是两个可以独立使用的I2C设备
    let axp173_i2c_proxy = bus_manager.acquire_i2c();
    let es8311_i2c_proxy = bus_manager.acquire_i2c();

    /* */
    // init_wifi(peripherals, sysloop, &app_config)?;
    {
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

    // 初始化 LCD 屏幕

    // 1. 配置LEDC定时器
    let timer_driver = LedcTimerDriver::new(
        peripherals.ledc.timer0,
        &TimerConfig::new().frequency(25000.Hz().into()),
    )
    .unwrap();

    let backlight_pin = pins.gpio8;

    // 2. 配置LEDC通道，并绑定到背光引脚
    let mut channel =
        LedcDriver::new(peripherals.ledc.channel0, timer_driver, backlight_pin).unwrap();

    // 3. 设置亮度 (通过设置占空比)
    let max_duty = channel.get_max_duty();
    // channel.set_duty(max_duty * 3 / 4).unwrap(); // 设置为50%的亮度

    // 初始化 ST7789 屏幕

    // // 2. 根据 diagram.json 配置引脚
    // // 控制引脚
    // // #define DISPLAY_MOSI_PIN      GPIO_NUM_4
    // // #define DISPLAY_CLK_PIN       GPIO_NUM_5
    // // #define DISPLAY_DC_PIN        GPIO_NUM_7
    // // #define DISPLAY_RST_PIN       GPIO_NUM_NC
    // // #define DISPLAY_CS_PIN        GPIO_NUM_6
    let dc = pins.gpio7;
    // SPI 总线引脚 (使用硬件 SPI2)
    let sck = pins.gpio5;
    let sdi = pins.gpio4; // MOSI 在驱动中通常被称为 SDI (Serial Data In)
    let sdo = Option::<AnyInputPin>::None; // MISO
    let cs = pins.gpio6; // 直接使用引脚，而不是PinDriver

    // 3. 初始化 SPI 驱动
    // 创建 SPI 驱动程序实例
    let driver = SpiDriver::new(
        spi3, // 使用 SPI3
        sck,
        sdi,
        sdo,
        &Default::default(),
    )
    .unwrap();

    lcd::LcdSt7789::init(driver, dc.into(), cs.into());

    // channel.set_duty(0).unwrap(); // 设置为50%的亮度

    // // show led demo
    // let led = pins.gpio38;
    // let channel: esp_idf_hal::rmt::CHANNEL0 = peripherals.rmt.channel0;
    // led_demo(led.into(), channel)

    /*
    // 初始化ILI9341
    let dc = pins.gpio4;
    let rst = pins.gpio8; // 即使 diagram.json 没连，驱动也需要这个对象

    // SPI 总线引脚 (使用硬件 SPI2)
    let sck = pins.gpio12;
    let sdi = pins.gpio11; // MOSI 在驱动中通常被称为 SDI (Serial Data In)
    let sdo = pins.gpio13; // MISO
    let cs = pins.gpio10; // 直接使用引脚，而不是PinDriver

    let driver: SpiDriver<'_> = SpiDriver::new(
        peripherals.spi2, // 使用 SPI2
        sck,
        sdi,
        Some(sdo),
        &Default::default(),
    )
    .unwrap();

    lcd::LcdIli9341::init(driver, dc.into(), rst.into(), cs.into());
    */

    //初始化es8311音频解码器
    let mut es8311 = audio::es8311::Es8311::new(es8311_i2c_proxy);

    // match es8311.read_u8(0xFD) {
    //     Ok(chip_id) => {
    //         info!("SUCCESS! Successfully read from ES8311.");
    //         info!("Chip ID: 0x{:02X} (should be 0x83)", chip_id);

    //         info!("Now attempting full init...");
    //     }
    //     Err(_) => {
    //         println!("FATAL: Failed to read from ES8311 at address 0x18.");
    //         println!("Please check: ");
    //         println!("  1. ES8311 Power Supply (is LDO3 correct?).");
    //         println!("  2. I2C Pin connections (GPIO1 for SDA, GPIO2 for SCL?).");
    //         println!("  3. Physical wiring and soldering.");
    //     }
    // }



    Delay::new_default().delay_ms(1000);

    let mut delay = Delay::new_default();

    // es8311.init(&mut delay).unwrap();

    match es8311.init(&mut delay) {
        Ok(_) => {
            println!("初始化ES8311成功");
        }
        Err(e) => {
            println!("初始化ES8311失败:{:?}", e);
            return;
        }
    }

    // 初始化I2S
    let std_config = StdConfig::philips(24000, DataBitWidth::Bits16);
    let peripherals = Peripherals::take().unwrap();
    let bclk = peripherals.pins.gpio42;
    let din = peripherals.pins.gpio45;
    let dout = peripherals.pins.gpio39;
    let mclk = peripherals.pins.gpio41.into();
    let ws = peripherals.pins.gpio40;
    let mut i2s_driver = I2sDriver::<I2sBiDir>::new_std_bidir(
        peripherals.i2s0,
        &std_config,
        bclk,
        din,
        dout,
        mclk,
        ws,
    )
    .unwrap();

    const PCM_DATA: &'static [u8] = include_bytes!("../assets/sound.pcm");

    info!(
        "Embedded PCM data size: {} bytes. Starting playback...",
        PCM_DATA.len()
    );
    match i2s_driver.write_all(PCM_DATA, BLOCK) {
        Ok(_) => info!("Playback finished successfully!"),
        Err(e) => println!("I2S write error: {:?}", e),
    }

    info!("Test complete. Entering infinite loop.");

    loop {
        FreeRtos::delay_ms(1000);
    }
}

fn led_demo(led_pin: gpio::AnyOutputPin, channel: esp_idf_hal::rmt::CHANNEL0) {
    let mut ws2812 = WS2812RMT::new(led_pin, channel).unwrap();
    loop {
        info!("Red!");
        ws2812.set_pixel(rgb::RGB8::new(255, 0, 0)).unwrap();
        FreeRtos::delay_ms(1000);
        info!("Green!");
        ws2812.set_pixel(rgb::RGB8::new(0, 255, 0)).unwrap();
        FreeRtos::delay_ms(1000);
        info!("Blue!");
        ws2812.set_pixel(rgb::RGB8::new(0, 0, 255)).unwrap();
        FreeRtos::delay_ms(1000);
    }
}
