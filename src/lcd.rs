use display_interface_spi::SPIInterfaceNoCS;
use embedded_graphics::{
    mono_font::{ascii::FONT_8X13, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    text::Text,
};
use esp_idf_hal::{
    delay::Delay,
    gpio::{Output, PinDriver},
    spi::{SpiAnyPins, SpiDeviceDriver, SpiDriver},
    units::*,
};
use esp_idf_hal::{gpio, spi::SpiConfig};
// 删除错误的导入
use mipidsi::{
    interface::SpiInterface,
    models::ILI9341Rgb565,
    options::{ColorInversion, Orientation, Rotation},
    Builder,
};
pub struct LcdIli9341;

pub struct SPIConfig<T: SpiAnyPins> {
    pub spi: T,
    pub clock_pin: gpio::AnyOutputPin<'static>,
    pub mosi_pin: gpio::AnyOutputPin<'static>,
    pub miso_pin: gpio::AnyOutputPin<'static>,
    pub chip_select_pin: gpio::AnyOutputPin<'static>,
    pub dc_pin: gpio::AnyOutputPin<'static>,
    pub reset_pin: gpio::AnyOutputPin<'static>,
}

impl LcdIli9341 {
    pub fn init(
        driver: SpiDriver<'_>,
        dc_pin: gpio::AnyOutputPin<'static>,
        reset_pin: gpio::AnyOutputPin<'static>,
        chip_select_pin: gpio::AnyOutputPin<'static>,
    ) {
        // 控制引脚
        let dc = PinDriver::<'static, esp_idf_hal::gpio::Output>::output(dc_pin).unwrap();
        let rst = PinDriver::<'static, esp_idf_hal::gpio::Output>::output(reset_pin).unwrap(); // 即使 diagram.json 没连，驱动也需要这个对象

        // // SPI 总线引脚 (使用硬件 SPI2)
        // let sck = spi_config.clock_pin;
        // let sdi = spi_config.mosi_pin; // MOSI 在驱动中通常被称为 SDI (Serial Data In)
        // let sdo = spi_config.miso_pin; // MISO
        // let cs = spi_config.chip_select_pin; // 直接使用引脚，而不是PinDriver

        // 3. 初始化 SPI 驱动
        // // 创建 SPI 驱动程序实例
        // let driver: SpiDriver<'_> = SpiDriver::new(
        //     peripherals.spi2, // 使用 SPI2
        //     sck,
        //     sdi,
        //     Some(sdo),
        //     &Default::default(),
        // )
        // .unwrap();

        // 创建一个 SPI 设备驱动，它包含了 CS 片选和通信速率等配置
        let spi_config = SpiConfig::new().baudrate(MegaHertz(40).into());
        let spi_device = SpiDeviceDriver::new(driver, Some(chip_select_pin), &spi_config).unwrap();

        println!("SPI 初始化完成!");

        // // 创建显示接口
        // let di = SPIInterface::new(spi_device, dc, cs);

        let buffer = [0_u8; 512];
        let box_buffer = Box::new(buffer);

        let static_buffer = Box::leak(box_buffer);
        let di = SpiInterface::new(spi_device, dc, static_buffer);
        let mut delay = Delay::new_default();
        let reset_pin: Option<esp_idf_hal::gpio::PinDriver<'static, esp_idf_hal::gpio::Output>> =
            Some(rst);
        // let mut display: mipidsi::Display<
        //     _,
        //     _,
        //     esp_idf_hal::gpio::PinDriver<'static, esp_idf_hal::gpio::Output>,
        // > = Builder::ili9341(di).init(&mut delay, reset_pin).unwrap(); // delay provider from your MCU

        let mut display: mipidsi::Display<
            SpiInterface<'_, SpiDeviceDriver<'_, SpiDriver<'_>>, PinDriver<'_, Output>>,
            ILI9341Rgb565,
            mipidsi::NoResetPin,
        > = Builder::new(ILI9341Rgb565, di)
            .display_size(320, 240)
            .invert_colors(ColorInversion::Inverted)
            .init(&mut delay)
            .unwrap();

        display.clear(Rgb565::BLACK).unwrap();
        display
            .set_orientation(Orientation::new().rotate(Rotation::Deg90))
            .unwrap();

        // 创建一个文本样式
        let style = MonoTextStyle::new(&FONT_8X13, Rgb565::WHITE);

        // 创建文本对象
        Text::new(
            "Hello, Rust!!",
            Point::new(70, 150), // 文本左上角在屏幕上的位置
            style,
        )
        .draw(&mut display) // 绘制文本
        .unwrap();

        println!("'Hello, Rust!' 已经显示在 LCD 上。");

        // display
    }
}
