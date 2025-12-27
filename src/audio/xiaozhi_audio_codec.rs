use esp_idf_hal::{delay::Delay, i2c::I2cDriver};
use log::info;

use crate::audio::{audio_codec::AudioCodec, es7210::es7210::Es7210, es8311::Es8311};

type I2cProxy = shared_bus::I2cProxy<'static, shared_bus::NullMutex<I2cDriver<'static>>>;
pub struct XiaozhiAudioCodec {
    input_codec: Es7210<I2cProxy>,
    output_codec: Es8311<I2cProxy>,
}

impl XiaozhiAudioCodec {
    pub fn new(es8311_i2c_proxy: I2cProxy, es7210_i2c_proxy: I2cProxy) -> Self {
        let mut es8311 = Es8311::new(es8311_i2c_proxy);
        let mut delay = Delay::new_default();
        match es8311.open(&mut delay) {
            Ok(_) => {
                println!("初始化ES8311成功");
            }
            Err(e) => {
                println!("初始化ES8311失败:{:?}", e);
                // return Err(anyhow!("初始化ES8311失败:{:?}", e));
            }
        }

        let mut es7210 = Es7210::new(es7210_i2c_proxy);
        info!("初始化ES7210...");

        match es7210.open() {
            Ok(_) => {
                println!("初始化ES8311成功");
            }
            Err(e) => {
                println!("初始化ES8311失败:{:?}", e);
                // return Err(anyhow!("初始化ES8311失败:{:?}", e));
            }
        }

        Self {
            input_codec: es7210,
            output_codec: es8311,
        }
    }
}

impl AudioCodec for XiaozhiAudioCodec {
    fn set_output_volume(&mut self, volume: u8) -> Result<(), anyhow::Error> {
        self.output_codec.set_voice_volume(volume)?;
        Ok(())
    }

    fn enable_input(&mut self, enable: bool) -> Result<(), anyhow::Error> {
        if enable {
            self.input_codec.enable()?;
        } else {
            self.input_codec.disable()?;
        }
        Ok(())
    }

    fn enable_output(&mut self, enable: bool) -> Result<(), anyhow::Error> {
        if enable {
            self.output_codec.enable()?;
        } else {
            self.output_codec.disable()?;
        }
        Ok(())
    }
}
