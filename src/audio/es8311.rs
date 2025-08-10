// src/es8311.rs

use embedded_hal::blocking::delay::DelayUs;
use embedded_hal::blocking::i2c::{Write, WriteRead};

// 定义错误类型，这里我们直接用I2C的错误类型
pub type Result<T, E> = core::result::Result<T, E>;

// ES8311 默认的I2C从机地址
const ADDR: u8 = 0x30;

// --- ES8311 寄存器地址定义 ---
// (这些地址来自ES8311数据手册)
const ES8311_RESET_REG: u8 = 0x00;
const ES8311_CLK_MANAGER_REG_01: u8 = 0x01;
const ES8311_CLK_MANAGER_REG_02: u8 = 0x02;
const ES8311_CLOCK_MANAGER_REG_04: u8 = 0x04;
const ES8311_CLOCK_MANAGER_REG_05: u8 = 0x05;
const ES8311_CHIP_POWER_REG: u8 = 0x06;
const ES8311_GPIO_REG: u8 = 0x07;
const ES8311_MASTER_MODE_REG: u8 = 0x08;
const ES8311_ADC_CONTROL_REG_01: u8 = 0x09;
const ES8311_DAC_CONTROL_REG_01: u8 = 0x12;
const ES8311_DAC_CONTROL_REG_02: u8 = 0x13;
const ES8311_DAC_CONTROL_REG_03: u8 = 0x14;
const ES8311_DAC_L_VOLUME_REG: u8 = 0x1B;
const ES8311_DAC_R_VOLUME_REG: u8 = 0x1C;
const ES8311_GPIO_REG_44: u8 = 0x44;

const ES8311_SYSTEM_REG_10: u8 = 0x10;
const ES8311_SYSTEM_REG_11: u8 = 0x11;
const ES8311_SYSTEM_REG_0B: u8 = 0x0B;
const ES8311_SYSTEM_REG_0C: u8 = 0x0C;
const ES8311_SYSTEM_REG_13: u8 = 0x13;

const ES8311_ADC_REG_1B: u8 = 0x1B;
const ES8311_ADC_REG_1C: u8 = 0x1C;

const ES8311_DAC_VOLUME_REG_32: u8 = 0x32;

/// 代表ES8311音频编解码器驱动
pub struct Es8311<I2C> {
    i2c: I2C,
}

impl<I2C, E> Es8311<I2C>
where
    I2C: Write<Error = E> + WriteRead<Error = E>,
{
    /// 创建一个新的ES8311驱动实例
    pub fn new(i2c: I2C) -> Self {
        Self { i2c }
    }

    /// 初始化CODEC芯片
    /// 这是最关键的函数，它按照datasheet的推荐序列来配置芯片
    pub fn init<D: DelayUs<u32>>(&mut self, delay: &mut D) -> Result<(), E> {
        // 1. 复位芯片
        self.write_reg(ES8311_RESET_REG, 0x80)?; // 复位数字部分
        delay.delay_us(50_000); // 等待50ms
                                // self.write_reg(ES8311_RESET_REG, 0x00)?; // 恢复正常

        /* Enhance ES8311 I2C noise immunity */
        self.write_reg(ES8311_GPIO_REG_44, 0x08)?; // 复位数字部分
                                                   /* Due to occasional failures during the first I2C write with the ES8311 chip, a second write is performed to ensure reliability */
        self.write_reg(ES8311_GPIO_REG_44, 0x08)?; // 复位数字部分

        // 2. 配置时钟
        self.write_reg(ES8311_CLK_MANAGER_REG_01, 0x30)?; // MCLK和BCLK使能
        self.write_reg(ES8311_CLK_MANAGER_REG_02, 0x00)?; // I2S为主模式, 16-bit
                                                          // self.write_reg(ES8311_MASTER_MODE_REG, 0x00)?; // I2S格式, 16-bit
                                                          // self.write_reg(ES8311_CHIP_POWER_REG, 0x00)?; // 全部电源开启
        self.write_reg(ES8311_CLOCK_MANAGER_REG_04, 0x10)?; // ADC电源开启
        self.write_reg(ES8311_CLOCK_MANAGER_REG_05, 0x00)?; // DAC电源开启

        //3. 配置电源

        self.write_reg(ES8311_SYSTEM_REG_10, 0x1F)?; //这是默认值
        self.write_reg(ES8311_SYSTEM_REG_11, 0x7F)?; //根据手册，这个reg的6:0是内部使用的，不知道小智为啥配置成0x7F

        self.write_reg(ES8311_SYSTEM_REG_0B, 0x00)?; //这是默认值
        self.write_reg(ES8311_SYSTEM_REG_0C, 0x00)?; //这是根据小智代码来设置的，这个不是默认值

        self.write_reg(ES8311_SYSTEM_REG_13, 0x10)?; //enable output to HP driver
        self.write_reg(ES8311_ADC_REG_1B, 0x0A)?;
        self.write_reg(ES8311_ADC_REG_1C, 0x6A)?;

        // 5. 设置默认音量 (0-33, 0是最大声, 33是静音)
        self.set_voice_volume(20)?; // 设置一个适中的音量

        Ok(())
    }

    /// 设置播放音量
    /// `volume`: 255 (最大) -  0(静音)
    pub fn set_voice_volume(&mut self, volume: u8) -> Result<(), E> {
        let mut percent = volume.min(100); // 确保值在范围内
        percent = volume.max(0);

        let vol = 255 as u16 * percent as u16 / 100 as u16; //255 = 0xFF - 0x00
        let value = vol as u8;

        self.write_reg(ES8311_DAC_VOLUME_REG_32, value)?;
        Ok(())
    }

    /// 设置静音
    pub fn set_mute(&mut self, mute: bool) -> Result<(), E> {
        if mute {
            self.set_voice_volume(0) // 设置为最大衰减即静音
        } else {
            // 这里可以恢复到之前的音量，或者一个默认音量
            self.set_voice_volume(20)
        }
    }

    /// Reads a single byte from a register.
    pub fn read_u8(&mut self, reg: u8) -> Result<u8, E> {
        let mut byte: [u8; 1] = [0; 1];

        match self.i2c.write_read(ADDR, &[reg], &mut byte) {
            Ok(_) => Ok(byte[0]),
            Err(e) => Err(e),
        }
    }

    /// 向一个寄存器写入一个字节
    fn write_reg(&mut self, reg: u8, value: u8) -> Result<(), E> {
        self.i2c.write(ADDR, &[reg, value])
    }
}
