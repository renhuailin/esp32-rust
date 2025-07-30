use display_interface_spi::SPIInterfaceNoCS;
use embedded_graphics::{
    mono_font::{ascii::FONT_8X13, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    text::Text,
};
use esp_idf_hal::{
    delay::Delay,
    gpio::PinDriver,
    prelude::*,
    spi::{SpiAnyPins, SpiDeviceDriver, SpiDriver},
    units::MegaHertz,
};
use esp_idf_hal::{gpio, spi::SpiConfig};
// 删除错误的导入
use mipidsi::{Builder, Orientation};

pub struct LcdSt7789;

impl LcdSt7789 {
    pub fn init(peripherals: Peripherals) {
        let pins = peripherals.pins;
        let spi3 = peripherals.spi3;
        // 2. 根据 diagram.json 配置引脚
        // 控制引脚
        // #define DISPLAY_MOSI_PIN      GPIO_NUM_4
        // #define DISPLAY_CLK_PIN       GPIO_NUM_5
        // #define DISPLAY_DC_PIN        GPIO_NUM_7
        // #define DISPLAY_RST_PIN       GPIO_NUM_NC
        // #define DISPLAY_CS_PIN        GPIO_NUM_6
        let dc =
            PinDriver::<esp_idf_hal::gpio::Gpio7, esp_idf_hal::gpio::Output>::output(pins.gpio7)
                .unwrap();

        // SPI 总线引脚 (使用硬件 SPI2)
        let sck = pins.gpio5;
        let sdi = pins.gpio4; // MOSI 在驱动中通常被称为 SDI (Serial Data In)
        let sdo = Option::<esp_idf_hal::gpio::Gpio13>::None; // MISO
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

        const rate: u32 = 80 * 1000 * 1000;
        // 创建一个 SPI 设备驱动，它包含了 CS 片选和通信速率等配置
        let spi_config = SpiConfig::new().baudrate(rate.Hz());
        let spi_device = SpiDeviceDriver::new(driver, Some(cs), &spi_config).unwrap();

        println!("SPI 初始化完成!");

        // // 创建显示接口
        // let di = SPIInterface::new(spi_device, dc, cs);

        let di = SPIInterfaceNoCS::new(spi_device, dc);

        let mut delay = Delay::new_default();
        let reset_pin: Option<
            esp_idf_hal::gpio::PinDriver<esp_idf_hal::gpio::Gpio7, esp_idf_hal::gpio::Output>,
        > = None;
        let mut display: mipidsi::Display<
            _,
            _,
            esp_idf_hal::gpio::PinDriver<esp_idf_hal::gpio::Gpio7, esp_idf_hal::gpio::Output>,
        > = Builder::st7789(di).init(&mut delay, reset_pin).unwrap(); // delay provider from your MCU

        display.clear(Rgb565::BLACK).unwrap();
        display
            .set_orientation(Orientation::Portrait(true))
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

pub struct LcdIli9341;

pub struct SPIConfig<T: SpiAnyPins> {
    pub spi: T,
    pub clock_pin: gpio::AnyOutputPin,
    pub mosi_pin: gpio::AnyOutputPin,
    pub miso_pin: gpio::AnyOutputPin,
    pub chip_select_pin: gpio::AnyOutputPin,
    pub dc_pin: gpio::AnyOutputPin,
    pub reset_pin: gpio::AnyOutputPin,
}

impl LcdIli9341 {
    pub fn init(
        driver: SpiDriver<'_>,
        dc_pin: gpio::AnyOutputPin,
        reset_pin: gpio::AnyOutputPin,
        chip_select_pin: gpio::AnyOutputPin,
    ) {
        // 控制引脚
        let dc =
            PinDriver::<gpio::AnyOutputPin, esp_idf_hal::gpio::Output>::output(dc_pin).unwrap();
        let rst =
            PinDriver::<gpio::AnyOutputPin, esp_idf_hal::gpio::Output>::output(reset_pin).unwrap(); // 即使 diagram.json 没连，驱动也需要这个对象

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

        let di = SPIInterfaceNoCS::new(spi_device, dc);

        let mut delay = Delay::new_default();
        let reset_pin: Option<
            esp_idf_hal::gpio::PinDriver<gpio::AnyOutputPin, esp_idf_hal::gpio::Output>,
        > = Some(rst);
        let mut display: mipidsi::Display<
            _,
            _,
            esp_idf_hal::gpio::PinDriver<gpio::AnyOutputPin, esp_idf_hal::gpio::Output>,
        > = Builder::ili9341_rgb565(di)
            .init(&mut delay, reset_pin)
            .unwrap(); // delay provider from your MCU

        display.clear(Rgb565::BLACK).unwrap();
        display
            .set_orientation(Orientation::Portrait(true))
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
