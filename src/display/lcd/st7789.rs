use crate::{common::qrcode::draw_qrcode, display::Display};
use anyhow::{Ok, Result};
use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
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
use log::info;
use mipidsi::{
    interface::SpiInterface,
    models::ST7789,
    options::{ColorInversion, Orientation, Rotation},
    Builder,
};
use u8g2_fonts::U8g2TextStyle;
// 1. 定义具体的硬件类型别名，方便阅读

// type ConcreteRstPin<'a> = PinDriver<'a, InputOutput>;

// 3. 定义最终的 Display 类型
// 注意：mipidsi::Display<接口, 型号, 复位引脚>
pub type St7789Display = mipidsi::Display<
    SpiInterface<'static, SpiDeviceDriver<'static, SpiDriver<'static>>, PinDriver<'static, Output>>,
    ST7789,
    mipidsi::NoResetPin,
>;

pub struct LcdSt7789 {
    display: St7789Display,
    ledc_driver: LedcDriver<'static>,
}

const W: i32 = 240;
const H: i32 = 320;

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
        // let di = SPIInterfaceNoCS::new(spi_device, dc);

        // --- 屏幕初始化 ---
        let mut delay = Delay::new_default();

        // 定义 Reset 引脚 (虽然是 None，但类型要对齐)
        // let reset_pin: Option<ConcreteRstPin> = None;

        // 3. 设置亮度 (通过设置占空比)
        let max_duty = ledc_driver.get_max_duty();
        ledc_driver.set_duty(max_duty * 0 / 4).unwrap(); // 设置为50%的亮度
                                                         // ledc_driver.set_duty(max_duty).unwrap();

        // let mut display = Builder::st7789(di)
        //     .with_display_size(320, 240)
        //     .with_color_order(ColorOrder::Rgb)
        //     .init(&mut delay, reset_pin)
        //     .map_err(|e| anyhow::anyhow!("Display init failed: {:?}", e))?;

        let buffer = [0_u8; 512];
        let box_buffer = Box::new(buffer);

        let static_buffer = Box::leak(box_buffer);
        let di = SpiInterface::new(spi_device, dc, static_buffer);

        let mut display: mipidsi::Display<
            SpiInterface<'_, SpiDeviceDriver<'_, SpiDriver<'_>>, PinDriver<'_, Output>>,
            ST7789,
            mipidsi::NoResetPin,
        > = Builder::new(ST7789, di)
            .display_size(W as u16, H as u16)
            .invert_colors(ColorInversion::Normal)
            .init(&mut delay)
            .unwrap();

        // --- 屏幕设置 ---
        // display
        //     .set_orientation(Orientation::LandscapeInverted(true))
        //     .map_err(|e| anyhow::anyhow!("Orientation failed: {:?}", e))?;

        display
            .set_orientation(Orientation::new().rotate(Rotation::Deg270))
            .map_err(|e| anyhow::anyhow!("Orientation failed: {:?}", e))?;

        display
            .clear(Rgb565::BLACK)
            .map_err(|e| anyhow::anyhow!("Clear failed: {:?}", e))?;

        info!("LcdSt7789 初始化完成");

        // 清屏
        // display.clear(Rgb565::BLACK).unwrap();

        // // 1. 画一个刚好 240x320 的红框
        // Rectangle::new(Point::new(0, 0), Size::new(320, 240))
        //     .into_styled(PrimitiveStyle::with_stroke(Rgb565::RED, 10))
        //     .draw(&mut display)
        //     .unwrap();

        // let character_style =
        //     U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_wqy12_t_gb2312, Rgb565::WHITE);

        // Text::new(
        //     "你好, Rust!!!!!!",
        //     Point::new(10, 10), // 文本左上角在屏幕上的位置
        //     character_style.clone(),
        // )
        // .draw(&mut display) // 绘制文本
        // .unwrap();

        // Text::new(
        //     "你好, Rust!!!!!!",
        //     Point::new(300, 230), // 文本左上角在屏幕上的位置
        //     character_style,
        // )
        // .draw(&mut display) // 绘制文本
        // .unwrap();

        // 返回结构体
        Ok(Self {
            display,
            ledc_driver,
        })
    }

    // pub fn init(
    //     driver: SpiDriver<'_>,
    //     dc_pin: gpio::AnyOutputPin<'static>,
    //     chip_select_pin: gpio::AnyOutputPin<'static>,
    // ) {
    //     // let pins = peripherals.pins;
    //     // let spi3 = peripherals.spi3;

    //     // // 2. 根据 diagram.json 配置引脚
    //     // // 控制引脚
    //     // // #define DISPLAY_MOSI_PIN      GPIO_NUM_4
    //     // // #define DISPLAY_CLK_PIN       GPIO_NUM_5
    //     // // #define DISPLAY_DC_PIN        GPIO_NUM_7
    //     // // #define DISPLAY_RST_PIN       GPIO_NUM_NC
    //     // // #define DISPLAY_CS_PIN        GPIO_NUM_6
    //     // let dc =
    //     //     PinDriver::<esp_idf_hal::gpio::Gpio7, esp_idf_hal::gpio::Output>::output(pins.gpio7)
    //     //         .unwrap();

    //     // // SPI 总线引脚 (使用硬件 SPI2)
    //     // let sck = pins.gpio5;
    //     // let sdi = pins.gpio4; // MOSI 在驱动中通常被称为 SDI (Serial Data In)
    //     // let sdo = Option::<esp_idf_hal::gpio::Gpio13>::None; // MISO
    //     // let cs = pins.gpio6; // 直接使用引脚，而不是PinDriver

    //     // // 3. 初始化 SPI 驱动
    //     // // 创建 SPI 驱动程序实例
    //     // let driver = SpiDriver::new(
    //     //     spi3, // 使用 SPI3
    //     //     sck,
    //     //     sdi,
    //     //     sdo,
    //     //     &Default::default(),
    //     // )
    //     // .unwrap();

    //     // --- 步骤 2: 打开背光电源 ---

    //     const RATE: u32 = 80 * 1000 * 1000;
    //     // 创建一个 SPI 设备驱动，它包含了 CS 片选和通信速率等配置
    //     let spi_config = SpiConfig::new().baudrate(RATE.Hz());
    //     let spi_device = SpiDeviceDriver::new(driver, Some(chip_select_pin), &spi_config).unwrap();

    //     println!("SPI 初始化完成!");

    //     // // 创建显示接口
    //     // let di = SPIInterface::new(spi_device, dc, cs);
    //     let dc = PinDriver::<'static, esp_idf_hal::gpio::Output>::output(dc_pin).unwrap();
    //     let di = SPIInterfaceNoCS::new(spi_device, dc);

    //     let mut delay = Delay::new_default();
    //     let reset_pin: Option<esp_idf_hal::gpio::PinDriver<'static, esp_idf_hal::gpio::Output>> =
    //         None;
    //     let mut display: mipidsi::Display<
    //         _,
    //         _,
    //         esp_idf_hal::gpio::PinDriver<'static, esp_idf_hal::gpio::Output>,
    //     > = Builder::st7789(di)
    //         .with_color_order(ColorOrder::Rgb)
    //         .init(&mut delay, reset_pin)
    //         .unwrap(); // delay provider from your MCU
    //     display
    //         .set_orientation(Orientation::LandscapeInverted(true))
    //         .unwrap();

    //     // 清屏
    //     display.clear(Rgb565::BLACK).unwrap();

    //     // 创建一个文本样式
    //     let style = MonoTextStyle::new(&FONT_8X13, Rgb565::WHITE);

    //     // 创建文本对象
    //     Text::new(
    //         "Hello, Rust!!",
    //         Point::new(30, 30), // 文本左上角在屏幕上的位置
    //         style,
    //     )
    //     .draw(&mut display) // 绘制文本
    //     .unwrap();

    //     println!("'Hello, Rust!' 已经显示在 LCD 上。");

    //     // display
    // }
}

impl Display for LcdSt7789 {
    fn set_status(&mut self, status: &str) {
        let clear_area = Rectangle::new(Point::new(60, 0), Size::new(200, 40));
        clear_area
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .fill_color(Rgb565::BLACK)
                    .build(),
            )
            .draw(&mut self.display)
            .unwrap();

        let character_style =
            U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312, Rgb565::WHITE);

        let mut text_width = 0;
        for ch in status.chars() {
            text_width += if ch.is_ascii() { 8 } else { 16 };
        }
        let x = 60 + (200 - text_width) / 2;
        let x = x.max(60).min(240) as i32;

        Text::new(status, Point::new(x, 24), character_style)
            .draw(&mut self.display)
            .unwrap();
    }
    fn show_qrcode(&mut self, content: &str) {
        let _code = draw_qrcode(
            &mut self.display,
            content,
            Point { x: 50, y: 50 },
            8,
            Rgb565::BLACK,
            Rgb565::WHITE,
        );
    }

    fn show_wifi_signal(&mut self, rssi: Option<i8>) {
        let clear_area = Rectangle::new(Point::new(0, 0), Size::new(60, 40));
        clear_area
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .fill_color(Rgb565::BLACK)
                    .build(),
            )
            .draw(&mut self.display)
            .unwrap();

        let bars = match rssi {
            None => 0,
            Some(r) if r >= -30 => 4,
            Some(r) if r >= -50 => 3,
            Some(r) if r >= -70 => 2,
            Some(r) if r >= -85 => 1,
            Some(_) => 0,
        };

        let bar_w: i32 = 6;
        let gap: i32 = 2;
        let base_x: i32 = 5;
        let base_y: i32 = 24; // 底部对齐在 y=24
        let max_h: i32 = 16; // 总高 16px（之前是 20，缩为与电池同高）

        for i in 0..4 {
            let h = (i + 1) * max_h / 4;
            let x = base_x + i * (bar_w + gap);
            let y = base_y - h;

            let color = if i < bars {
                match bars {
                    1 | 2 => Rgb565::RED,
                    3 => Rgb565::YELLOW,
                    _ => Rgb565::GREEN,
                }
            } else {
                Rgb565::new(0x20, 0x20, 0x20)
            };

            Rectangle::new(Point::new(x, y), Size::new(bar_w as u32, h as u32))
                .into_styled(PrimitiveStyleBuilder::new().fill_color(color).build())
                .draw(&mut self.display)
                .unwrap();
        }
    }

    fn show_battery_level(&mut self, level: u8) {
        let clear_area = Rectangle::new(Point::new(260, 0), Size::new(60, 40));
        clear_area
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .fill_color(Rgb565::BLACK)
                    .build(),
            )
            .draw(&mut self.display)
            .unwrap();

        let level = level.min(100);

        let bat_x = 275;
        let bat_y = 11; // 从 8 改为 11，使电池底部落在 y=25
        let bat_w = 30;
        let bat_h = 14;

        Rectangle::new(Point::new(bat_x, bat_y), Size::new(bat_w, bat_h))
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .stroke_color(Rgb565::WHITE)
                    .stroke_width(1)
                    .fill_color(Rgb565::BLACK)
                    .build(),
            )
            .draw(&mut self.display)
            .unwrap();

        Rectangle::new(Point::new(bat_x + bat_w as i32, bat_y + 4), Size::new(3, 6))
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .fill_color(Rgb565::WHITE)
                    .build(),
            )
            .draw(&mut self.display)
            .unwrap();

        if level > 0 {
            let max_fill = (bat_w - 4) as u32;
            let fill_w = (max_fill * level as u32 / 100).max(1);
            let fill_color = if level > 60 {
                Rgb565::GREEN
            } else if level > 20 {
                Rgb565::YELLOW
            } else {
                Rgb565::RED
            };

            Rectangle::new(
                Point::new(bat_x + 2, bat_y + 2),
                Size::new(fill_w, (bat_h - 4) as u32),
            )
            .into_styled(PrimitiveStyleBuilder::new().fill_color(fill_color).build())
            .draw(&mut self.display)
            .unwrap();
        }
    }
}
