use std::sync::{Arc, Mutex};

use crate::{
    audio::codec::{audio_codec::AudioCodec, es7210::es7210::Es7210, es8311::Es8311},
    setting::nvs_setting::NvsSetting,
};
use anyhow::{Error, Result};
use esp_idf_hal::{
    delay::Delay,
    i2c::I2cDriver,
    i2s::{I2sBiDir, I2sDriver},
};
use log::{error, info};

type I2cProxy = shared_bus::I2cProxy<'static, Mutex<I2cDriver<'static>>>;

pub struct XiaozhiAudioCodec {
    input_codec: Es7210<I2cProxy>,
    output_codec: Es8311<I2cProxy>,
    input_enabled: bool,
    output_enabled: bool,
    output_volume: u8,
    i2s_driver: Arc<Mutex<I2sDriver<'static, I2sBiDir>>>,
}

impl XiaozhiAudioCodec {
    pub fn new(
        es8311_i2c_proxy: I2cProxy,
        es7210_i2c_proxy: I2cProxy,
        i2s_driver: I2sDriver<'static, I2sBiDir>,
    ) -> Self {
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
            input_enabled: false,
            output_enabled: false,
            output_volume: 0,
            i2s_driver: Arc::new(Mutex::new(i2s_driver)),
        }
    }
}

impl AudioCodec for XiaozhiAudioCodec {
    fn set_output_volume(&mut self, volume: u8) -> Result<(), anyhow::Error> {
        self.output_codec.set_voice_volume(volume)?;
        Ok(())
    }

    fn enable_input(&mut self, enable: bool) -> Result<(), anyhow::Error> {
        if enable == self.input_enabled {
            return Ok(());
        }

        if enable {
            self.input_codec.enable()?;
        } else {
            self.input_codec.disable()?;
        }
        Ok(())
    }

    fn enable_output(&mut self, enable: bool) -> Result<(), anyhow::Error> {
        if enable == self.output_enabled {
            return Ok(());
        }
        if enable {
            self.output_codec.enable()?;
        } else {
            self.output_codec.disable()?;
        }
        Ok(())
    }

    fn input_enabled(&self) -> bool {
        return self.input_enabled;
    }

    fn output_enabled(&self) -> bool {
        return self.output_enabled;
    }

    fn start(&mut self) {
        match NvsSetting::new("audio") {
            Ok(setting) => {
                if let Some(volume) = setting.get_u8("output_volume") {
                    self.output_volume = volume;
                }
            }
            Err(_) => {
                error!("Failed to get audio setting");
            }
        }
        let i2s_driver_arc = self.i2s_driver.clone();
        let mut i2s_driver = i2s_driver_arc.lock().unwrap();
        i2s_driver.tx_enable().unwrap();
        i2s_driver.rx_enable().unwrap();

        self.enable_input(true);
        self.enable_output(true);
        info!("Audio codec started");
    }

    fn read_audio_data(&mut self, mut buffer: &mut Vec<u8>) -> Result<usize, Error> {
        let i2s_driver_arc = self.i2s_driver.clone();
        let mut i2s_driver = i2s_driver_arc.lock().unwrap();
        let bytes_read = i2s_driver.read(&mut buffer, 50)?;
        return Ok(bytes_read);
    }
}
