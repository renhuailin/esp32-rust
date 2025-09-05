use crate::audio::es7210::enums::MicGain;
use crate::audio::es7210::reg::*;
use crate::utils::bits::update_bit;
use anyhow::Result;
use embedded_hal::blocking::i2c::{Write, WriteRead};
use esp_idf_hal::i2c::I2cError;

// 1. 在文件顶部引入 thiserror::Error
use thiserror::Error;

// 2. 定义一个新的、更强大的Error枚举
#[derive(Error, Debug)]
pub enum Error {
    #[error("Underlying I2C communication error")]
    I2c(#[from] I2cError), // <-- #[from] 属性会自动实现 From<E> for Error<E>

    #[error("Invalid microphone input mode provided")] // <-- #[error] 属性提供了人类可读的描述
    InvalidMicInputMode,
}

const ADDR: u8 = 0x18;

pub struct Es7210<I2C> {
    i2c: I2C,
    is_open: bool,
    enabled: bool,
    clock_off_status: u8,
}

impl<I2C> Es7210<I2C>
where
    I2C: Write<Error = I2cError> + WriteRead<Error = I2cError>,
{
    /// 创建一个新的ES7210驱动实例
    pub fn new(i2c: I2C) -> Self {
        Self {
            i2c,
            is_open: false,
            enabled: false,
            clock_off_status: 0,
        }
    }

    pub fn open(&mut self) -> Result<(), Error> {
        // 配置I2C设备
        self.i2c.write(0x01, &[0x01])?;

        // ret |= es7210_write_reg(codec, ES7210_RESET_REG00, 0xff);//0xff=11111111b. Reset all registers.
        // ret |= es7210_write_reg(codec, ES7210_RESET_REG00, 0x41); //0x41=01000001b. reset master mode LRCK and SCLK,
        self.write_reg(ES7210_RESET_REG_00, 0xff)?; // 复位数字部分
        self.write_reg(ES7210_RESET_REG_00, 0x41)?; //0x41=01000001b reset master mode LRCK and SCLK, and enable Chip state machine power down

        // es7210_write_reg(codec, ES7210_CLOCK_OFF_REG01, 0x3f);
        self.write_reg(ES7210_CLOCK_OFF_REG_01, 0x3f)?; //0x3f=00111111b. turn off  clocks

        // ret |= es7210_write_reg(codec, ES7210_TIME_CONTROL0_REG09, 0x30); /* Set chip state cycle */
        self.write_reg(ES7210_TIME_CONTROL0_REG_09, 0x30)?; //0x30=00110000b. Set chip state cycle
                                                            // reg 0x09 CHIPINI_LGTH 7:0 Chip initial state period control:
                                                            // period=CHIPINI_LGTH/(LRCK frequency)*16

        /* 根据 es7210 芯片手册的描述，reg 0x0A 的 Power up state period 是指 ES7210 芯片从上电（Power Up）或复位（Reset）后，进入正常工作状态（Active）之前，所处的一个过渡状态的持续时间。
        这个过渡状态通常用于：
        稳定电路：让内部的振荡器（Oscillator）、参考电压（Reference Voltage）以及其他模拟电路有足够的时间稳定下来，达到正常工作所需的精确度。
        初始化：在芯片开始处理数据之前，完成内部寄存器的初始化设置。
        简单来说，Power up state period 就是给芯片一个“准备时间”，确保它在真正开始工作时，所有的内部系统都已经处于最佳状态。你可以通过 reg 0x0A 的配置来调整这个准备时间的长度，以适应不同的应用需求和外部电路的特性。
        这个参数对于确保芯片的稳定性和可靠性非常重要。如果这个时间太短，芯片可能在未完全稳定时就开始工作，导致性能不稳定或产生错误数据。 */
        self.write_reg(ES7210_TIME_CONTROL0_REG_0A, 0x30)?; //0x30=00110000b. Set power on state cycle

        // ret |= es7210_write_reg(codec, ES7210_ADC12_HPF2_REG23, 0x2a);    /* Quick setup */
        // ret |= es7210_write_reg(codec, ES7210_ADC12_HPF1_REG22, 0x0a);
        // ret |= es7210_write_reg(codec, ES7210_ADC34_HPF2_REG20, 0x0a);
        // ret |= es7210_write_reg(codec, ES7210_ADC34_HPF1_REG21, 0x2a);
        self.write_reg(ES7210_ADC12_HPF2_REG_20, 0x0a)?; //0x0a=00001010
        self.write_reg(ES7210_ADC12_HPF2_REG_21, 0x2a)?; //0x2a=00101010
        self.write_reg(ES7210_ADC12_HPF2_REG_22, 0x0a)?;
        self.write_reg(ES7210_ADC12_HPF2_REG_23, 0x2a)?;

        self.set_master_mode(false)?;

        // /* Select power off analog, vdda = 3.3V, close vx20ff, VMID select 5KΩ start */
        // ret |= es7210_write_reg(codec, ES7210_ANALOG_REG40, 0x43);
        // ret |= es7210_write_reg(codec, ES7210_MIC12_BIAS_REG41, 0x70); /* Select 2.87v */
        // ret |= es7210_write_reg(codec, ES7210_MIC34_BIAS_REG42, 0x70); /* Select 2.87v */
        // ret |= es7210_write_reg(codec, ES7210_OSR_REG07, 0x20);
        self.write_reg(ES7210_ANALOG_REG_40, 0x43)?; // 0x43=0b01000011
        self.write_reg(ES7210_ANALOG_REG_41, 0x70)?; // 0x70=0b01110000. Select 2.87v
        self.write_reg(ES7210_ANALOG_REG_42, 0x70)?; // 0x70=0b01110000. Select 2.87v
        self.write_reg(ES7210_OSR_REG_07, 0x20)?;

        /* Set the frequency division coefficient and use dll except clock doubler, and need to set 0xc1 to clear the state */
        // ret |= es7210_write_reg(codec, ES7210_MAINCLK_REG02, 0xc1);
        self.write_reg(ES7210_MAINCLK_REG_02, 0xc1)?; // 0xc1=0b11000001.reg 0x02:
                                                      // 7: 1 – bypass DLL 0 – not bypass DLL
                                                      // 6: 1 – use clock doubler 0 – not use clock doubler;
                                                      // 5: Reserved
                                                      // 4:0 ADC clock divide 0/1 – no divide 2 – divide by 2
        let input_mics =
            ES7210_INPUT_MIC1 | ES7210_INPUT_MIC2 | ES7210_INPUT_MIC3 | ES7210_INPUT_MIC4;
        self.mic_select(input_mics)?;
        self.set_channel_gain(input_mics, 0xF, 30.0)?;

        //把clcok off的设置保存起来,在enbale的时候需要用到。
        self.clock_off_status = self.read_reg(ES7210_CLOCK_OFF_REG_01)?;
        self.is_open = true;
        Ok(())
    }

    pub fn start(&mut self) -> Result<(), Error> {
        // int ret = 0;
        // ret |= es7210_write_reg(codec, ES7210_CLOCK_OFF_REG01, clock_reg_value);
        // ret |= es7210_write_reg(codec, ES7210_POWER_DOWN_REG06, 0x00);
        // ret |= es7210_write_reg(codec, ES7210_ANALOG_REG40, 0x43);
        // ret |= es7210_write_reg(codec, ES7210_MIC1_POWER_REG47, 0x08);
        // ret |= es7210_write_reg(codec, ES7210_MIC2_POWER_REG48, 0x08);
        // ret |= es7210_write_reg(codec, ES7210_MIC3_POWER_REG49, 0x08);
        // ret |= es7210_write_reg(codec, ES7210_MIC4_POWER_REG4A, 0x08);
        // ret |= es7210_mic_select(codec, codec->mic_select);
        // ret |= es7210_write_reg(codec, ES7210_ANALOG_REG40, 0x43);
        // ret |= es7210_write_reg(codec, ES7210_RESET_REG00, 0x71);
        // ret |= es7210_write_reg(codec, ES7210_RESET_REG00, 0x41);

        self.write_reg(ES7210_CLOCK_OFF_REG_01, self.clock_off_status)?;
        self.write_reg(ES7210_POWER_DOWN_REG_06, 0x00)?;
        self.write_reg(ES7210_ANALOG_REG_40, 0x43)?;
        self.write_reg(ES7210_MIC1_POWER_REG_47, 0x08)?; //只有PGA是Low power，其余的为normal
        self.write_reg(ES7210_MIC2_POWER_REG_48, 0x08)?;
        self.write_reg(ES7210_MIC3_POWER_REG_49, 0x08)?;
        self.write_reg(ES7210_MIC4_POWER_REG_4A, 0x08)?;

        let input_mics =
            ES7210_INPUT_MIC1 | ES7210_INPUT_MIC2 | ES7210_INPUT_MIC3 | ES7210_INPUT_MIC4;
        self.mic_select(input_mics)?;

        self.write_reg(ES7210_ANALOG_REG_40, 0x43)?;

        self.write_reg(ES7210_RESET_REG_00, 0x71)?; // 0x71=0b01110001
        self.write_reg(ES7210_RESET_REG_00, 0x41)?; //0x41=01000001b reset master mode LRCK and SCLK, and enable Chip state machine power down

        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), Error> {
        // ret |= es7210_write_reg(codec, ES7210_MIC1_POWER_REG47, 0xff);
        // ret |= es7210_write_reg(codec, ES7210_MIC2_POWER_REG48, 0xff);
        // ret |= es7210_write_reg(codec, ES7210_MIC3_POWER_REG49, 0xff);
        // ret |= es7210_write_reg(codec, ES7210_MIC4_POWER_REG4A, 0xff);
        // ret |= es7210_write_reg(codec, ES7210_MIC12_POWER_REG4B, 0xff);
        // ret |= es7210_write_reg(codec, ES7210_MIC34_POWER_REG4C, 0xff);
        // ret |= es7210_write_reg(codec, ES7210_ANALOG_REG40, 0xc0);
        // ret |= es7210_write_reg(codec, ES7210_CLOCK_OFF_REG01, 0x7f);
        // ret |= es7210_write_reg(codec, ES7210_POWER_DOWN_REG06, 0x07);

        self.write_reg(ES7210_MIC1_POWER_REG_47, 0xff)?;
        self.write_reg(ES7210_MIC2_POWER_REG_48, 0xff)?;
        self.write_reg(ES7210_MIC3_POWER_REG_49, 0xff)?;
        self.write_reg(ES7210_MIC4_POWER_REG_4A, 0xff)?;
        self.write_reg(ES7210_MIC12_POWER_DOWN_REG_4B, 0xff)?;
        self.write_reg(ES7210_MIC34_POWER_DOWN_REG_4C, 0xff)?;
        self.write_reg(ES7210_ANALOG_REG_40, 0xc0)?;
        self.write_reg(ES7210_CLOCK_OFF_REG_01, 0x7f)?;
        self.write_reg(ES7210_POWER_DOWN_REG_06, 0x07)?;

        Ok(())
    }

    pub fn enable(&mut self) -> Result<(), Error> {
        self.enabled = true;

        Ok(())
    }

    pub fn disable(&mut self) -> Result<(), Error> {
        self.enabled = false;

        Ok(())
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 设置工作模式为master
    /// 参数：
    /// master_mode: true为master模式，false为slave模式
    fn set_master_mode(&mut self, master_mode: bool) -> Result<(), Error> {
        // if (codec_cfg->master_mode) {
        //     ESP_LOGI(TAG, "Work in Master mode");
        //     ret |= es7210_update_reg_bit(codec, ES7210_MODE_CONFIG_REG08, 0x01, 0x01);
        //     /* Select clock source for internal mclk */
        //     switch (codec_cfg->mclk_src) {
        //         case ES7210_MCLK_FROM_PAD:
        //         default:
        //             ret |= es7210_update_reg_bit(codec, ES7210_MASTER_CLK_REG03, 0x80, 0x00);
        //             break;
        //         case ES7210_MCLK_FROM_CLOCK_DOUBLER:
        //             ret |= es7210_update_reg_bit(codec, ES7210_MASTER_CLK_REG03, 0x80, 0x80);
        //             break;
        //     }
        // } else {
        //     ESP_LOGI(TAG, "Work in Slave mode");
        //     ret |= es7210_update_reg_bit(codec, ES7210_MODE_CONFIG_REG08, 0x01, 0x00);
        // }

        let mut regv = self.read_reg(ES7210_MODE_CONFIG_REG_08)?;
        if master_mode {
            println!("es7210 Work in Master mode");
            regv |= 0x01;
        } else {
            println!("es7210 Work in Slave mode");
            regv &= !(0x01);
        }
        self.write_reg(ES7210_MODE_CONFIG_REG_08, regv)?;
        Ok(())
    }

    /// Reads a single byte from a register.
    pub fn read_reg(&mut self, reg: u8) -> Result<u8, Error> {
        let mut byte: [u8; 1] = [0; 1];

        match self.i2c.write_read(ADDR, &[reg], &mut byte) {
            Ok(_) => Ok(byte[0]),
            Err(e) => Err(Error::I2c(e)),
        }
    }

    //private methods
    /// 向一个寄存器写入一个字节
    fn write_reg(&mut self, reg: u8, value: u8) -> Result<(), Error> {
        self.i2c.write(ADDR, &[reg, value]).map_err(Error::I2c)
    }

    fn set_channel_gain(&mut self, input_mics: u8, channel_mask: u8, db: f32) -> Result<(), Error> {
        //     int ret = 0;
        //     es7210_gain_value_t gain = get_db(db);
        //     if ((codec->mic_select & ES7210_INPUT_MIC1) & (channel_mask & ESP_CODEC_DEV_MAKE_CHANNEL_MASK(0)))
        //     {
        //         ret |= es7210_update_reg_bit(codec, ES7210_MIC1_GAIN_REG43, 0x0f, gain);
        //     }
        //     if ((codec->mic_select & ES7210_INPUT_MIC2) & (channel_mask & ESP_CODEC_DEV_MAKE_CHANNEL_MASK(1)))
        //     {
        //         ret |= es7210_update_reg_bit(codec, ES7210_MIC2_GAIN_REG44, 0x0f, gain);
        //     }
        //     if ((codec->mic_select & ES7210_INPUT_MIC3) & (channel_mask & ESP_CODEC_DEV_MAKE_CHANNEL_MASK(2)))
        //     {
        //         ret |= es7210_update_reg_bit(codec, ES7210_MIC3_GAIN_REG45, 0x0f, gain);
        //     }
        //     if ((codec->mic_select & ES7210_INPUT_MIC4) & (channel_mask & ESP_CODEC_DEV_MAKE_CHANNEL_MASK(3)))
        //     {
        //         ret |= es7210_update_reg_bit(codec, ES7210_MIC4_GAIN_REG46, 0x0f, gain);
        //     }
        //     return ret == 0 ? ESP_CODEC_DEV_OK : ESP_CODEC_DEV_WRITE_FAIL;
        let gain = self.get_db(db);
        if (input_mics & ES7210_INPUT_MIC1) & (channel_mask & self.make_channel_mask(0)) != 0 {
            self.update_reg_bit(ES7210_MIC1_GAIN_REG_43, 0x0f, gain)?;
        }

        if (input_mics & ES7210_INPUT_MIC2) & (channel_mask & self.make_channel_mask(1)) != 0 {
            self.update_reg_bit(ES7210_MIC2_GAIN_REG_44, 0x0f, gain)?;
        }

        if (input_mics & ES7210_INPUT_MIC3) & (channel_mask & self.make_channel_mask(2)) != 0 {
            self.update_reg_bit(ES7210_MIC3_GAIN_REG_45, 0x0f, gain)?;
        }

        if (input_mics & ES7210_INPUT_MIC4) & (channel_mask & self.make_channel_mask(3)) != 0 {
            self.update_reg_bit(ES7210_MIC4_GAIN_REG_46, 0x0f, gain)?;
        }

        Ok(())
    }

    fn make_channel_mask(&self, channel: u8) -> u8 {
        1 << channel
    }

    fn get_db(&mut self, fdb: f32) -> u8 {
        let db = fdb + 0.5;
        if db < 33.0 {
            let index: u8 = if db < 3.0 { 0 } else { (db / 3.0) as u8 };
            return MicGain::Gain0db.value() + index;
        }
        if db < 34.5 {
            return MicGain::Gain30db.value();
        }
        if db < 36.0 {
            return MicGain::Gain34_5db.value();
        }
        if db < 37.0 {
            return MicGain::Gain36db.value();
        }
        return MicGain::Gain37_5db.value();
    }

    fn mic_select(&mut self, input_mics: u8) -> Result<(), Error> {
        if (input_mics
            & (ES7210_INPUT_MIC1 | ES7210_INPUT_MIC2 | ES7210_INPUT_MIC3 | ES7210_INPUT_MIC4))
            != 0
        {
            // es7210_update_reg_bit(codec, ES7210_MIC1_GAIN_REG43 + i, 0x10, 0x00);
            self.update_reg_bit(ES7210_MIC1_GAIN_REG_43, 0x10, 0x00)?; //把第4位置为0，deselect MIC1,下面三行依次deselect MIC2 MIC3 MIC4
            self.update_reg_bit(ES7210_MIC2_GAIN_REG_44, 0x10, 0x00)?;
            self.update_reg_bit(ES7210_MIC3_GAIN_REG_45, 0x10, 0x00)?;
            self.update_reg_bit(ES7210_MIC4_GAIN_REG_46, 0x10, 0x00)?;

            // if (codec->mic_select & ES7210_INPUT_MIC1)
            // {
            //     ESP_LOGI(TAG, "Enable ES7210_INPUT_MIC1");
            //     ret |= es7210_update_reg_bit(codec, ES7210_CLOCK_OFF_REG01, 0x0b, 0x00);
            //     ret |= es7210_write_reg(codec, ES7210_MIC12_POWER_REG4B, 0x00);
            //     ret |= es7210_update_reg_bit(codec, ES7210_MIC1_GAIN_REG43, 0x10, 0x10);
            //     ret |= es7210_update_reg_bit(codec, ES7210_MIC1_GAIN_REG43, 0x0f, codec->gain);
            // }

            if (input_mics & ES7210_INPUT_MIC1) != 0 {
                self.update_reg_bit(ES7210_CLOCK_OFF_REG_01, 0x0b, 0x00)?; //0x0b=0b1011,
                                                                           //turn on master clock
                                                                           //turn on ADC12 analog clock
                                                                           //turn on ADC12 master clock
                self.write_reg(ES7210_MIC12_POWER_DOWN_REG_4B, 0x00)?; //打开mic1,mic2的电源
                self.update_reg_bit(ES7210_MIC1_GAIN_REG_43, 0x10, 0x10)?;
                self.update_reg_bit(ES7210_MIC1_GAIN_REG_43, 0x0f, 0)?;
            }

            if (input_mics & ES7210_INPUT_MIC2) != 0 {
                self.update_reg_bit(ES7210_CLOCK_OFF_REG_01, 0x0b, 0x00)?; //0x0b=0b1011,
                                                                           //turn on master clock
                                                                           //turn on ADC12 analog clock
                                                                           //turn on ADC12 master clock
                self.write_reg(ES7210_MIC12_POWER_DOWN_REG_4B, 0x00)?; //打开mic1,mic2的电源
                self.update_reg_bit(ES7210_MIC2_GAIN_REG_44, 0x10, 0x10)?;
                self.update_reg_bit(ES7210_MIC2_GAIN_REG_44, 0x0f, 0)?;
            }

            if (input_mics & ES7210_INPUT_MIC3) != 0 {
                // ret |= es7210_update_reg_bit(codec, ES7210_CLOCK_OFF_REG01, 0x15, 0x00);
                // ret |= es7210_write_reg(codec, ES7210_MIC34_POWER_REG4C, 0x00);
                self.update_reg_bit(ES7210_CLOCK_OFF_REG_01, 0x15, 0x00)?; //0x15=0b00010101
                                                                           //turn on master clock
                                                                           //turn on ADC34 analog clock
                                                                           //turn on ADC34 master clock
                self.write_reg(ES7210_MIC34_POWER_DOWN_REG_4C, 0x00)?; //打开MIC3/4的电源
                self.update_reg_bit(ES7210_MIC3_GAIN_REG_45, 0x10, 0x10)?;
                self.update_reg_bit(ES7210_MIC3_GAIN_REG_45, 0x0f, 0)?;
            }

            if (input_mics & ES7210_INPUT_MIC4) != 0 {
                self.update_reg_bit(ES7210_CLOCK_OFF_REG_01, 0x15, 0x00)?; //0x15=0b00010101
                                                                           //turn on master clock
                                                                           //turn on ADC34 analog clock
                                                                           //turn on ADC34 master clock
                self.write_reg(ES7210_MIC34_POWER_DOWN_REG_4C, 0x00)?; //打开MIC3/4的电源
                self.update_reg_bit(ES7210_MIC4_GAIN_REG_46, 0x10, 0x10)?;
                self.update_reg_bit(ES7210_MIC4_GAIN_REG_46, 0x0f, 0)?;
            }
        } else {
            return Err(Error::InvalidMicInputMode);
        }

        // if (es7210_is_tdm_mode(codec)) {
        //     ret |= es7210_write_reg(codec, ES7210_SDP_INTERFACE2_REG12, 0x02);
        //     ESP_LOGI(TAG, "Enable TDM mode");
        // } else {
        //     ret |= es7210_write_reg(codec, ES7210_SDP_INTERFACE2_REG12, 0x00);
        // }

        //确定es7210不是工作在tdm模式下
        self.write_reg(ES7210_SDP_INTERFACE2_REG_12, 0x00)?;

        Ok(())
    }

    fn update_reg_bit(&mut self, reg_addr: u8, update_bits: u8, data: u8) -> Result<(), Error> {
        //  int regv = 0;
        // es7210_read_reg(codec, reg_addr, &regv);
        // regv = (regv & (~update_bits)) | (update_bits & data);
        // return es7210_write_reg(codec, reg_addr, regv);
        let mut regv: u8 = self.read_reg(reg_addr)?;
        // regv = (regv & (!update_bits)) | (update_bits & data);

        regv = update_bit(regv, update_bits, data);

        self.write_reg(reg_addr, regv)?;
        Ok(())
    }
}
