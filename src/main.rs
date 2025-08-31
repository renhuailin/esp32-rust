use std::num::{NonZero, NonZeroU32};
use std::sync::Arc;
use std::{thread, time::Duration};

use esp_idf_hal::delay;
use esp_idf_hal::gpio::{Input, InterruptType, Pull};
use esp_idf_hal::task::notification::Notification;
use esp_idf_hal::task::thread::ThreadSpawnConfiguration;
use esp_idf_hal::{
    delay::{Delay, FreeRtos, BLOCK},
    gpio::{self, AnyIOPin, AnyInputPin, AnyOutputPin, PinDriver},
    i2c::{I2cConfig, I2cDriver},
    i2s::{
        config::{Config, DataBitWidth, Role, StdConfig},
        I2sBiDir, I2sDriver,
    },
    ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver},
    prelude::*,
    rmt::RmtChannel,
    spi::SpiDriver,
    task::block_on,
};
use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::{eventloop::EspSystemEventLoop, timer::EspTaskTimerService};
use esp_idf_sys::EspError;
use esp_idf_test2::{
    audio,
    axp173::{Axp173, Ldo},
    common::button::{self, Button},
    lcd,
    led::WS2812RMT,
    wifi::wifi,
};
use futures::{select, FutureExt};
use log::info;
use mipidsi::error;
use shared_bus::BusManagerSimple;

// 定义一个统一的事件枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEvent {
    ButtonPressed(u8), // u8可以代表按钮的GPIO号
                       // 未来可以添加其他事件，比如：
                       // NetworkPacketReceived,
                       // TimerElapsed,
}

/// 这是一个只负责设置中断的函数
/// 它接收一个Sender，当中断发生时，它会通过这个Sender发送消息

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
    channel.set_duty(0).unwrap(); // 设置为100%的亮度

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

    //关闭背光

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
    let std_config = StdConfig::philips(16000, DataBitWidth::Bits16);
    // let peripherals = Peripherals::take().unwrap();
    let bclk = pins.gpio42;
    let din = pins.gpio45;
    let dout = pins.gpio39;
    let mclk = pins.gpio41.into();
    let ws = pins.gpio40;

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

    // let mut i2s_driver =
    //     I2sDriver::new_std_tx(peripherals.i2s0, &std_config, bclk, dout, mclk, ws).unwrap();

    i2s_driver.tx_enable().unwrap();
    // let mut i2s_driver =
    //     I2sDriver::new_std_tx(peripherals.i2s0, &std_config, bclk, dout, mclk, ws).unwrap();

    println!("初始化I2S完成！");

    match es8311.enable() {
        Ok(_) => {
            println!("成功启动音频解码器");
        }
        Err(e) => {
            println!("启动音频解码器失败:{:?}", e);
            return;
        }
    }

    // play_audio(i2s_driver);

    //  定时熄屏
    let once_timer = EspTaskTimerService::new()
        .unwrap()
        .timer(move || {
            channel.set_duty(max_duty).unwrap(); //关闭背光
            info!("One-shot timer triggered");
        })
        .unwrap();

    once_timer.after(Duration::from_secs(2)).unwrap();

    thread::sleep(Duration::from_secs(3));

    info!("Test complete. Entering infinite loop.");

    // let touch_button = Box::new(button::Button::new(pins.gpio0).unwrap());
    // let volume_button = button::Button::new(pins.gpio47).unwrap();

    // 2. 启动一个专门的“中断监听线程”
    //    我们将引脚的所有权和Sender的克隆版本移动到这个线程
    // let touch_pin = pins.gpio0;
    // let volume_pin = pins.gpio47;
    // let sender_clone = sender.clone();

    // 4. (重要) PinDriver的所有权必须被维持
    //    我们将它们放入一个Box中并leak，以确保它们永久存在
    // Box::leak(Box::new(button1_pin));
    // Box::leak(Box::new(button2_pin));

    // println!("{}", result);
    // block_on(async move {
    //     // println!("Buttons initialized. Waiting for press...");
    //     loop {
    //         select! {
    //             _ = touch_button.wait().fuse()  => {
    //                 println!("touch_button 1 pressed!");
    //             },
    //             _ = volume_button.wait().fuse() => {
    //                 println!("volume_button 2 pressed!");
    //             },
    //         }
    //     }
    // });
    // init_buttons(touch_button, volume_button);

    let mut touch_button = Box::new(button::Button::new(pins.gpio0).unwrap());
    let volume_button = button::Button::new(pins.gpio47).unwrap();

    info!("Waiting for button press...");
    block_on(async move {
        // println!("Buttons initialized. Waiting for press...");
        loop {
            select! {
                _ = touch_button.wait().fuse()  => {
                    println!("touch_button 1 pressed!");
                    touch_button.enable_interrupt().unwrap();
                },
                _ = volume_button.wait().fuse() => {
                    println!("volume_button 2 pressed!");
                },
            }
        }
    });

    // loop {
    //     // FreeRtos::delay_ms(1000);
    //     std::thread::sleep(std::time::Duration::from_secs(1));
    // }
}

fn init_buttons(touch_button: Button<'_>, volume_button: Button<'_>) {
    // let touch_button = button::Button::new(touch_button_pin).unwrap();
    // let volume_button = button::Button::new(volume_button_pin).unwrap();
    block_on(async move {
        // println!("Buttons initialized. Waiting for press...");
        loop {
            select! {
                _ = touch_button.wait().fuse()  => {
                    println!("touch_button 1 pressed!");
                },
                _ = volume_button.wait().fuse() => {
                    println!("volume_button 2 pressed!");
                },
            }
        }
    });
}

fn play_audio(mut i2s_driver: I2sDriver<'_, I2sBiDir>) {
    const PCM_DATA: &'static [u8] = include_bytes!("../assets/sound.pcm");

    info!(
        "Embedded PCM data size: {} bytes. Starting playback...",
        PCM_DATA.len()
    );
    // match i2s_driver.write_all(PCM_DATA, BLOCK) {
    //     Ok(_) => info!("Playback finished successfully!"),
    //     Err(e) => println!("I2S write error: {:?}", e),
    // }

    const CHUNK_SIZE: usize = 4096;

    info!("Starting playback in chunks of {} bytes...", CHUNK_SIZE);

    // // 3. 使用 .chunks() 方法将整个PCM数据切分成多个小块
    for chunk in PCM_DATA.chunks(CHUNK_SIZE) {
        // 4. 逐块写入I2S驱动
        //    i2s_driver.write() 会阻塞，直到这一小块数据被成功写入DMA
        match i2s_driver.write(chunk, BLOCK) {
            Ok(bytes_written) => {
                // 打印一些进度信息，方便调试
                info!("Successfully wrote {} bytes to I2S.", bytes_written);
            }
            Err(e) => {
                // 如果在写入过程中出错，打印错误并跳出循环
                info!("I2S write error on a chunk: {:?}", e);
                break;
            }
        }
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
