// src/es8311.rs
use crate::audio::es8311::reg::*;
use embedded_hal::blocking::delay::DelayUs;
use embedded_hal::blocking::i2c::{Write, WriteRead};
// 定义错误类型，这里我们直接用I2C的错误类型
pub type Result<T, E> = core::result::Result<T, E>;

// ES8311 默认的I2C从机地址
const ADDR: u8 = 0x18;

/// 代表ES8311音频编解码器驱动
pub struct Es8311<I2C> {
    i2c: I2C,
    is_open: bool,
}

impl<I2C, E> Es8311<I2C>
where
    I2C: Write<Error = E> + WriteRead<Error = E>,
{
    /// 创建一个新的ES8311驱动实例
    pub fn new(i2c: I2C) -> Self {
        Self {
            i2c,
            is_open: false,
        }
    }

    /// 初始化CODEC芯片
    /// 这是最关键的函数，它按照datasheet的推荐序列来配置芯片
    pub fn init<D: DelayUs<u32>>(&mut self, delay: &mut D) -> Result<(), E> {
        println!("开始初始化ES8311");

        // 1. 复位芯片
        /* Enhance ES8311 I2C noise immunity */
        self.write_reg(ES8311_GPIO_REG_44, 0x08)?; // 复位数字部分
        delay.delay_us(50_000); // 等待50ms
                                /* Due to occasional failures during the first I2C write with the ES8311 chip, a second write is performed to ensure reliability */
        self.write_reg(ES8311_GPIO_REG_44, 0x08)?; // 复位数字部分

        // 2. 配置时钟
        self.write_reg(ES8311_CLK_MANAGER_REG_01, 0x30)?; // MCLK和BCLK使能
        self.write_reg(ES8311_CLK_MANAGER_REG_02, 0x00)?; // I2S为主模式, 16-bit
        self.write_reg(ES8311_CLK_MANAGER_REG_03, 0x10)?;
        self.write_reg(ES8311_ADC_REG_16, 0x24)?;

        self.write_reg(ES8311_CLOCK_MANAGER_REG_04, 0x10)?; // ADC电源开启
        self.write_reg(ES8311_CLOCK_MANAGER_REG_05, 0x00)?; // DAC电源开启

        //3. 配置电源
        self.write_reg(ES8311_SYSTEM_REG_0B, 0x00)?; //这是默认值
        self.write_reg(ES8311_SYSTEM_REG_0C, 0x00)?; //这是根据小智代码来设置的，这个不是默认值

        self.write_reg(ES8311_SYSTEM_REG_10, 0x1F)?; //这是默认值
        self.write_reg(ES8311_SYSTEM_REG_11, 0x7F)?; //根据手册，这个reg的6:0是内部使用的，不知道小智为啥配置成0x7F

        self.write_reg(ES8311_RESET_REG_00, 0x80)?;

        // set es8311 to slave mode
        self.set_master_mode(false)?;

        self.configure_mclk_source(true)?;

        self.set_invert_mclk(false)?;

        self.set_invert_sclk(false)?;

        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG13, 0x10);
        // ret |= es8311_write_reg(codec, ES8311_ADC_REG1B, 0x0A);
        // ret |= es8311_write_reg(codec, ES8311_ADC_REG1C, 0x6A);

        self.write_reg(ES8311_SYSTEM_REG_13, 0x10)?; //enable output to HP driver
        self.write_reg(ES8311_ADC_REG_1B, 0x0A)?;
        self.write_reg(ES8311_ADC_REG_1C, 0x6A)?;

        // if (codec_cfg->no_dac_ref == false) {
        //     /* set internal reference signal (ADCL + DACR) */
        //     ESP_LOGI(TAG, "no_dac_ref == false");
        //     ret |= es8311_write_reg(codec, ES8311_GPIO_REG44, 0x58);
        // } else {
        //     ESP_LOGI(TAG, "no_dac_ref == true");
        //     ret |= es8311_write_reg(codec, ES8311_GPIO_REG44, 0x08);
        // }
        self.write_reg(ES8311_GPIO_REG_44, 0x58)?;

        // 5. 设置默认音量 (0-33, 0是最大声, 33是静音)
        self.set_voice_volume(50)?; // 设置一个适中的音量

        Ok(())
    }

    pub fn enable(&mut self) -> Result<(), E> {
        // int ret = ESP_CODEC_DEV_OK;
        // int adc_iface = 0, dac_iface = 0;
        // int regv = 0x80;
        // if (codec->cfg.master_mode) {
        //     regv |= 0x40;
        // } else {
        //     regv &= 0xBF;
        // }
        // ret |= es8311_write_reg(codec, ES8311_RESET_REG00, regv);

        self.set_master_mode(false);

        // regv = 0x3F;
        // if (codec->cfg.use_mclk) {
        //     regv &= 0x7F;
        // } else {
        //     regv |= 0x80;
        // }
        // if (codec->cfg.invert_mclk) {
        //     regv |= 0x40;
        // } else {
        //     regv &= ~(0x40);
        // }
        // ret |= es8311_write_reg(codec, ES8311_CLK_MANAGER_REG01, regv);
        self.configure_mclk_source(true)?;

        self.set_invert_mclk(false)?;

        self.set_invert_sclk(false)?;

        // ret = es8311_read_reg(codec, ES8311_SDPIN_REG09, &dac_iface);
        // ret |= es8311_read_reg(codec, ES8311_SDPOUT_REG0A, &adc_iface);
        // if (ret != ESP_CODEC_DEV_OK) {
        //     return ret;
        // }
        // dac_iface &= 0xBF;
        // adc_iface &= 0xBF;
        // adc_iface |= BITS(6);
        // dac_iface |= BITS(6);
        // int codec_mode = codec->cfg.codec_mode;
        // if (codec_mode == ESP_CODEC_DEV_WORK_MODE_LINE) {
        //     ESP_LOGE(TAG, "Codec not support LINE mode");
        //     return ESP_CODEC_DEV_NOT_SUPPORT;
        // }
        // if (codec_mode == ESP_CODEC_DEV_WORK_MODE_ADC || codec_mode == ESP_CODEC_DEV_WORK_MODE_BOTH) {
        //     adc_iface &= ~(BITS(6));
        // }
        // if (codec_mode == ESP_CODEC_DEV_WORK_MODE_DAC || codec_mode == ESP_CODEC_DEV_WORK_MODE_BOTH) {
        //     dac_iface &= ~(BITS(6));
        // }

        // ret |= es8311_write_reg(codec, ES8311_SDPIN_REG09, dac_iface);
        // ret |= es8311_write_reg(codec, ES8311_SDPOUT_REG0A, adc_iface);

        // ret |= es8311_write_reg(codec, ES8311_ADC_REG17, 0xBF);
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG0E, 0x02);
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG12, 0x00);
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG14, 0x1A);

        // // pdm dmic enable or disable
        // regv = 0;
        // ret |= es8311_read_reg(codec, ES8311_SYSTEM_REG14, &regv);
        // if (codec->cfg.digital_mic) {
        //     regv |= 0x40;
        // } else {
        //     regv &= ~(0x40);
        // }
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG14, regv);
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG0D, 0x01);
        // ret |= es8311_write_reg(codec, ES8311_ADC_REG15, 0x40);
        // ret |= es8311_write_reg(codec, ES8311_DAC_REG37, 0x08);
        // ret |= es8311_write_reg(codec, ES8311_GP_REG45, 0x00);
        // return ret;

        // 在小智的main/audio_codecs/box_audio_codec.cc，我们可以查到，codec的工作模式为： ESP_CODEC_DEV_WORK_MODE_DAC
        // ret = es8311_read_reg(codec, ES8311_SDPIN_REG09, &dac_iface);
        // ret |= es8311_read_reg(codec, ES8311_SDPOUT_REG0A, &adc_iface);
        // if (ret != ESP_CODEC_DEV_OK) {
        //     return ret;
        // }
        // dac_iface &= 0xBF;
        // adc_iface &= 0xBF;
        // adc_iface |= BITS(6);
        // dac_iface |= BITS(6);
        // int codec_mode = codec->cfg.codec_mode;
        // if (codec_mode == ESP_CODEC_DEV_WORK_MODE_LINE) {
        //     ESP_LOGE(TAG, "Codec not support LINE mode");
        //     return ESP_CODEC_DEV_NOT_SUPPORT;
        // }
        // if (codec_mode == ESP_CODEC_DEV_WORK_MODE_ADC || codec_mode == ESP_CODEC_DEV_WORK_MODE_BOTH) {
        //     adc_iface &= ~(BITS(6));
        // }
        // if (codec_mode == ESP_CODEC_DEV_WORK_MODE_DAC || codec_mode == ESP_CODEC_DEV_WORK_MODE_BOTH) {
        //     dac_iface &= ~(BITS(6));
        // }

        // ret |= es8311_write_reg(codec, ES8311_SDPIN_REG09, dac_iface);
        // ret |= es8311_write_reg(codec, ES8311_SDPOUT_REG0A, adc_iface);

        let mut dac_iface = self.read_u8(ES8311_SDPIN_REG_09)?;
        let mut adc_iface = self.read_u8(ES8311_SDPOUT_REG_0A)?;

        dac_iface &= 0xBF; // 0xBF=10111111,把第6位清零。0x09寄存器的第6位是SDP in mute
                           // 0 – unmute (default)
                           // 1 – mute
                           //所以这一步是unmute,也就是取消静音。

        adc_iface &= 0xBF; // 0xBF=10111111,把第6位清零。0x0A寄存器的第6位是SDP out mute
                           // 0 – unmute (default)
                           // 1 – mute静音。

        // reg09v &= !(0x06);

        adc_iface |= 0x06; // 0x06=00000110,把第1位和第2位置1。 我看了手册，真不明白这是什么意思，因为1:0 和 4:2 是分开组表示一组意思的。下面的0x0A也是。
        dac_iface |= 0x06; // 0x06=00000110,把第1位和第2位置1。

        dac_iface &= !(0x06); // 0x06=00000110,把第1位和第2位置0。 如果是ESP_CODEC_DEV_WORK_MODE_DAC，要执行这个。
        dac_iface = 0x0C; // 0x0C=00001100,把第3位和第4位置1,16khz
        println!(
            "es8311 SDP IN REG 09: {}=0x{:X}={:08b}",
            dac_iface, dac_iface, dac_iface
        );
        self.write_reg(ES8311_SDPIN_REG_09, dac_iface)?;
        self.write_reg(ES8311_SDPOUT_REG_0A, adc_iface)?;

        // ret |= es8311_write_reg(codec, ES8311_ADC_REG17, 0xBF);
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG0E, 0x02);
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG12, 0x00);
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG14, 0x1A);

        self.write_reg(ES8311_ADC_REG17, 0xBF)?;
        self.write_reg(ES8311_SYSTEM_REG_0E, 0x02)?;
        self.write_reg(ES8311_SYSTEM_REG_12, 0x00)?;
        self.write_reg(ES8311_SYSTEM_REG_14, 0x1A)?;

        // // pdm dmic enable or disable
        // regv = 0;
        // ret |= es8311_read_reg(codec, ES8311_SYSTEM_REG14, &regv);
        // if (codec->cfg.digital_mic) {
        //     regv |= 0x40;
        // } else {
        //     regv &= ~(0x40);
        // }
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG14, regv);

        let mut regv = self.read_u8(ES8311_SYSTEM_REG_14)?; //这个值不是刚赋的吗？
        regv &= !(0x40);
        self.write_reg(ES8311_SYSTEM_REG_14, regv)?;

        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG0D, 0x01);
        // ret |= es8311_write_reg(codec, ES8311_ADC_REG15, 0x40);
        // ret |= es8311_write_reg(codec, ES8311_DAC_REG37, 0x08);
        // ret |= es8311_write_reg(codec, ES8311_GP_REG45, 0x00);

        self.write_reg(ES8311_SYSTEM_REG_0D, 0x01)?;
        self.write_reg(ES8311_ADC_REG_15, 0x40)?;
        self.write_reg(ES8311_DAC_REG_37, 0x08)?;
        self.write_reg(ES8311_GP_REG_45, 0x00)?;

        self.is_open = true;

        Ok(())
    }

    pub fn suspend(&mut self) -> Result<(), E> {
        // int ret = es8311_write_reg(codec, ES8311_DAC_REG32, 0x00);
        // ret |= es8311_write_reg(codec, ES8311_ADC_REG17, 0x00);
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG0E, 0xFF);
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG12, 0x02);
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG14, 0x00);
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG0D, 0xFA);
        // ret |= es8311_write_reg(codec, ES8311_ADC_REG15, 0x00);
        // ret |= es8311_write_reg(codec, ES8311_CLK_MANAGER_REG02, 0x10);
        // ret |= es8311_write_reg(codec, ES8311_RESET_REG00, 0x00);
        // ret |= es8311_write_reg(codec, ES8311_RESET_REG00, 0x1F);
        // ret |= es8311_write_reg(codec, ES8311_CLK_MANAGER_REG01, 0x30);
        // ret |= es8311_write_reg(codec, ES8311_CLK_MANAGER_REG01, 0x00);
        // ret |= es8311_write_reg(codec, ES8311_GP_REG45, 0x00);
        // ret |= es8311_write_reg(codec, ES8311_SYSTEM_REG0D, 0xFC);
        // ret |= es8311_write_reg(codec, ES8311_CLK_MANAGER_REG02, 0x00);
        // return ret;

        self.write_reg(ES8311_DAC_VOLUME_REG_32, 0x00)?; // set DAC volume to -95.5dB (default) mute
        self.write_reg(ES8311_ADC_REG_17, 0x00)?; // set ADC volume to -95.5dB (default)
        self.write_reg(ES8311_SYSTEM_REG_0E, 0xFF)?; // power down PGA ADC reset modulator
        self.write_reg(ES8311_SYSTEM_REG_12, 0x02)?; // power down DAC, disable  internal reference circuits for DAC output (default)

        self.write_reg(ES8311_SYSTEM_REG_14, 0x00)?;
        self.write_reg(ES8311_SYSTEM_REG_0D, 0xFA)?; //关闭内部各种电路的电源。只enable analog DAC reference circuits
        self.write_reg(ES8311_ADC_REG_15, 0x00)?;
        self.write_reg(ES8311_CLK_MANAGER_REG_02, 0x10)?;
        self.write_reg(ES8311_RESET_REG_00, 0x00)?;
        self.write_reg(ES8311_RESET_REG_00, 0x1F)?; //先置零，再恢复为默认值
        self.write_reg(ES8311_CLK_MANAGER_REG_01, 0x30)?; //MCLK on BCLK on
        self.write_reg(ES8311_CLK_MANAGER_REG_01, 0x00)?; // MCLK off BCLK off
        self.write_reg(ES8311_GP_REG_45, 0x00)?; // BCLK/LRCK  pullup on
        self.write_reg(ES8311_SYSTEM_REG_0D, 0xFC)?; //关闭内部各种电路的电源。保持vmid power down
        self.write_reg(ES8311_CLK_MANAGER_REG_02, 0x00)?;

        Ok(())
    }

    pub fn disable(&mut self) -> Result<(), E> {
        self.close()
    }

    pub fn close(&mut self) -> Result<(), E> {
        // audio_codec_es8311_t *codec = (audio_codec_es8311_t *) h;
        // if (codec == NULL) {
        //     return ESP_CODEC_DEV_INVALID_ARG;
        // }
        // if (codec->is_open) {
        //     es8311_suspend(codec);
        //     es8311_pa_power(codec, ES_PA_DISABLE);
        //     codec->is_open = false;
        // }

        if self.is_open {
            self.suspend()?;
            self.pa_power(false)?;
            self.is_open = false;
        }

        Ok(())
    }

    /// 设置播放音量
    /// `volume`: 100 (最大) -  0(静音)
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

    //private methods
    /// 向一个寄存器写入一个字节
    fn write_reg(&mut self, reg: u8, value: u8) -> Result<(), E> {
        self.i2c.write(ADDR, &[reg, value])
    }

    /// 设置工作模式为master
    /// 参数：
    /// master_mode: true为master模式，false为slave模式
    fn set_master_mode(&mut self, master_mode: bool) -> Result<(), E> {
        // ret = es8311_read_reg(codec, ES8311_RESET_REG00, &regv);
        // if (codec_cfg->master_mode) {
        //     ESP_LOGI(TAG, "Work in Master mode");
        //     regv |= 0x40;
        // } else {
        //     ESP_LOGI(TAG, "Work in Slave mode");
        //     regv &= 0xBF;
        // }
        // ret |= es8311_write_reg(codec, ES8311_RESET_REG00, regv);

        let mut regv = self.read_u8(ES8311_RESET_REG_00)?;
        if master_mode {
            println!("es8311 Work in Master mode");
            regv |= 0x40;
        } else {
            println!("es8311 Work in Slave mode");
            regv &= 0xBF;
            println!("es8311 SCLK: {}=0x{:X}={:08b}", regv, regv, regv);
        }
        self.write_reg(ES8311_RESET_REG_00, regv)?;
        Ok(())
    }

    fn set_invert_mclk(&mut self, invert: bool) -> Result<(), E> {
        let mut regv = self.read_u8(ES8311_CLK_MANAGER_REG_01)?;
        if invert {
            regv |= 0x40;
        } else {
            regv &= !(0x40);
        }

        self.write_reg(ES8311_CLK_MANAGER_REG_01, regv)?;
        Ok(())
    }

    fn set_invert_sclk(&mut self, invert: bool) -> Result<(), E> {
        // SCLK inverted or not
        // ret |= es8311_read_reg(codec, ES8311_CLK_MANAGER_REG06, &regv);
        // if (codec_cfg->invert_sclk) {
        //     ESP_LOGI(TAG, "invert sclk");
        //     regv |= 0x20;
        // } else {
        //     ESP_LOGI(TAG, "not invert sclk");
        //     regv &= ~(0x20);
        // }
        // ret |= es8311_write_reg(codec, ES8311_CLK_MANAGER_REG06, regv);

        let mut regv = self.read_u8(ES8311_CLOCK_MANAGER_REG_06)?;
        if invert {
            println!("es8311 invert sclk");
            regv |= 0x20;
        } else {
            println!("es8311 not invert sclk");
            regv &= !(0x20);
        }

        self.write_reg(ES8311_CLOCK_MANAGER_REG_06, regv)?;
        Ok(())
    }

    fn configure_mclk_source(&mut self, use_mclk: bool) -> Result<(), E> {
        let mut regv = 0x3F;
        if use_mclk {
            regv &= 0x7F;
        } else {
            regv |= 0x80;
        }
        self.write_reg(ES8311_CLK_MANAGER_REG_01, regv)?;
        Ok(())
    }

    /// 因为es8311的由axp 173来控制的，所以先不实现它。
    /// PA 通常指的是 Power Amplifier（功率放大器）
    fn pa_power(&self, enable: bool) -> Result<(), E> {
        let _ = enable;
        Ok(())
    }
}
