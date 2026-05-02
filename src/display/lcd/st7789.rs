use crate::{common::qrcode::draw_qrcode, display::Display};
use anyhow::{Ok, Result};
use display_interface_spi::SPIInterfaceNoCS;
use embedded_graphics::{
    mono_font::{ascii::FONT_8X13, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    text::Text,
};
use esp_idf_hal::gpio::*;
use esp_idf_hal::{
    delay::Delay,
    gpio::{self, PinDriver},
    ledc::LedcDriver,
    spi::{SpiConfig, SpiDeviceDriver, SpiDriver},
    units::*,
};
use mipidsi::{Builder, ColorOrder, Orientation};
use u8g2_fonts::U8g2TextStyle;
// 1. 定义具体的硬件类型别名，方便阅读
type ConcreteSpiDriver = SpiDeviceDriver<'static, SpiDriver<'static>>;
type ConcreteDcPin<'a> = PinDriver<'a, Output>;
type ConcreteRstPin<'a> = PinDriver<'a, InputOutput>;

// 2. 定义显示接口类型
type DisplayInterface<'a> = SPIInterfaceNoCS<ConcreteSpiDriver, ConcreteDcPin<'a>>;

// 3. 定义最终的 Display 类型
// 注意：mipidsi::Display<接口, 型号, 复位引脚>
pub type St7789Display =
    mipidsi::Display<DisplayInterface<'static>, mipidsi::models::ST7789, ConcreteRstPin<'static>>;

pub struct LcdSt7789 {
    display: St7789Display,
    ledc_driver: LedcDriver<'static>,
}

impl LcdSt7789 {
    pub fn new(
        driver: SpiDriver<'static>, // 注意这里改为 'static
        dc_pin: gpio::AnyOutputPin<'static>,
        chip_select_pin: gpio::AnyOutputPin<'static>,
        mut ledc_driver: LedcDriver<'static>,
    ) -> Result<Self> {
        // --- SPI 配置 ---
        const RATE: u32 = 80 * 1000 * 1000;
        let spi_config = SpiConfig::new().baudrate(RATE.Hz());
        let spi_device = SpiDeviceDriver::new(driver, Some(chip_select_pin), &spi_config)?;

        println!("SPI 初始化完成!");

        // --- 接口配置 ---
        let dc = PinDriver::output(dc_pin)?;
        let di = SPIInterfaceNoCS::new(spi_device, dc);

        // --- 屏幕初始化 ---
        let mut delay = Delay::new_default();

        // 定义 Reset 引脚 (虽然是 None，但类型要对齐)
        let reset_pin: Option<ConcreteRstPin> = None;

        // 3. 设置亮度 (通过设置占空比)
        let max_duty = ledc_driver.get_max_duty();
        ledc_driver.set_duty(max_duty * 0 / 4).unwrap(); // 设置为50%的亮度
                                                         // ledc_driver.set_duty(max_duty).unwrap();

        let mut display = Builder::st7789(di)
            .with_color_order(ColorOrder::Rgb)
            .init(&mut delay, reset_pin)
            .map_err(|e| anyhow::anyhow!("Display init failed: {:?}", e))?;

        // --- 屏幕设置 ---
        display
            .set_orientation(Orientation::LandscapeInverted(true))
            .map_err(|e| anyhow::anyhow!("Orientation failed: {:?}", e))?;

        display
            .clear(Rgb565::BLACK)
            .map_err(|e| anyhow::anyhow!("Clear failed: {:?}", e))?;

        println!("LcdSt7789 结构体初始化完成");

        // 清屏
        display.clear(Rgb565::BLACK).unwrap();

        let character_style =
            U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_wqy12_t_gb2312, Rgb565::WHITE);

        Text::new(
            "你好, Rust!!!!!!",
            Point::new(30, 10), // 文本左上角在屏幕上的位置
            character_style,
        )
        .draw(&mut display) // 绘制文本
        .unwrap();

        println!("'Hello, Rust!' 已经显示在 LCD 上。");

        // 返回结构体
        Ok(Self {
            display,
            ledc_driver,
        })
    }

    pub fn init(
        driver: SpiDriver<'_>,
        dc_pin: gpio::AnyOutputPin<'static>,
        chip_select_pin: gpio::AnyOutputPin<'static>,
    ) {
        // let pins = peripherals.pins;
        // let spi3 = peripherals.spi3;

        // // 2. 根据 diagram.json 配置引脚
        // // 控制引脚
        // // #define DISPLAY_MOSI_PIN      GPIO_NUM_4
        // // #define DISPLAY_CLK_PIN       GPIO_NUM_5
        // // #define DISPLAY_DC_PIN        GPIO_NUM_7
        // // #define DISPLAY_RST_PIN       GPIO_NUM_NC
        // // #define DISPLAY_CS_PIN        GPIO_NUM_6
        // let dc =
        //     PinDriver::<esp_idf_hal::gpio::Gpio7, esp_idf_hal::gpio::Output>::output(pins.gpio7)
        //         .unwrap();

        // // SPI 总线引脚 (使用硬件 SPI2)
        // let sck = pins.gpio5;
        // let sdi = pins.gpio4; // MOSI 在驱动中通常被称为 SDI (Serial Data In)
        // let sdo = Option::<esp_idf_hal::gpio::Gpio13>::None; // MISO
        // let cs = pins.gpio6; // 直接使用引脚，而不是PinDriver

        // // 3. 初始化 SPI 驱动
        // // 创建 SPI 驱动程序实例
        // let driver = SpiDriver::new(
        //     spi3, // 使用 SPI3
        //     sck,
        //     sdi,
        //     sdo,
        //     &Default::default(),
        // )
        // .unwrap();

        // --- 步骤 2: 打开背光电源 ---

        const RATE: u32 = 80 * 1000 * 1000;
        // 创建一个 SPI 设备驱动，它包含了 CS 片选和通信速率等配置
        let spi_config = SpiConfig::new().baudrate(RATE.Hz());
        let spi_device = SpiDeviceDriver::new(driver, Some(chip_select_pin), &spi_config).unwrap();

        println!("SPI 初始化完成!");

        // // 创建显示接口
        // let di = SPIInterface::new(spi_device, dc, cs);
        let dc = PinDriver::<'static, esp_idf_hal::gpio::Output>::output(dc_pin).unwrap();
        let di = SPIInterfaceNoCS::new(spi_device, dc);

        let mut delay = Delay::new_default();
        let reset_pin: Option<esp_idf_hal::gpio::PinDriver<'static, esp_idf_hal::gpio::Output>> =
            None;
        let mut display: mipidsi::Display<
            _,
            _,
            esp_idf_hal::gpio::PinDriver<'static, esp_idf_hal::gpio::Output>,
        > = Builder::st7789(di)
            .with_color_order(ColorOrder::Rgb)
            .init(&mut delay, reset_pin)
            .unwrap(); // delay provider from your MCU
        display
            .set_orientation(Orientation::LandscapeInverted(true))
            .unwrap();

        // 清屏
        display.clear(Rgb565::BLACK).unwrap();

        // 创建一个文本样式
        let style = MonoTextStyle::new(&FONT_8X13, Rgb565::WHITE);

        // 创建文本对象
        Text::new(
            "Hello, Rust!!",
            Point::new(30, 30), // 文本左上角在屏幕上的位置
            style,
        )
        .draw(&mut display) // 绘制文本
        .unwrap();

        println!("'Hello, Rust!' 已经显示在 LCD 上。");

        // display
    }
}

impl Display for LcdSt7789 {
    fn set_status(&mut self, status: &str) {
        // 创建一个文本样式
        let style = MonoTextStyle::new(&FONT_8X13, Rgb565::WHITE);

        // 创建文本对象
        Text::new(
            status,
            Point::new(30, 30), // 文本左上角在屏幕上的位置
            style,
        )
        .draw(&mut self.display) // 绘制文本
        .unwrap();

        println!("'Hello, Rust!' 已经显示在 LCD 上。");
    }

    fn show_qrcode(&mut self, content: &str) {
        let _code = draw_qrcode(
            &mut self.display,
            content,
            Point { x: 30, y: 30 },
            6,
            Rgb565::BLACK,
            Rgb565::WHITE,
        );
    }
}
