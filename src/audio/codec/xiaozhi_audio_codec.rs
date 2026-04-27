use std::sync::{Arc, Mutex};

use crate::{
    audio::codec::{
        audio_codec::AudioCodec, es7210::es7210::Es7210, es8311::Es8311,
        opus::decoder::OpusAudioDecoder,
    },
    setting::nvs_setting::NvsSetting,
};
use anyhow::{Error, Result};
use esp_idf_hal::{
    delay::{Delay, BLOCK},
    i2c::I2cDriver,
    i2s::{I2sBiDir, I2sDriver},
};
use log::{error, info};

type I2cProxy = shared_bus::I2cProxy<'static, Mutex<I2cDriver<'static>>>;
const DEFAULT_OUTPUT_VOLUME: u8 = 30;
pub struct XiaozhiAudioCodec {
    input_codec: Es7210<I2cProxy>,
    output_codec: Es8311<I2cProxy>,
    input_enabled: bool,
    output_enabled: bool,
    output_volume: u8,
    i2s_driver: Arc<Mutex<I2sDriver<'static, I2sBiDir>>>,
    input_reference: bool,
    input_channels: i32,
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
                println!("初始化es7210成功");
            }
            Err(e) => {
                println!("初始化es7210失败:{:?}", e);
                // return Err(anyhow!("初始化es7210失败:{:?}", e));
            }
        }
        let input_reference = true;
        // let input_channels = input_reference ? 2 : 1;

        //一共就两个channels,只有input_reference时才会使用两个channels，而且要是全双工的才行。
        let input_channels = if input_reference { 2 } else { 1 };

        Self {
            input_codec: es7210,
            output_codec: es8311,
            input_enabled: false,
            output_enabled: false,
            output_volume: 0,
            i2s_driver: Arc::new(Mutex::new(i2s_driver)),
            input_reference: input_reference,
            input_channels,
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
                    if volume <= 0 {
                        self.output_volume = DEFAULT_OUTPUT_VOLUME;
                    } else {
                        self.output_volume = volume;
                    }
                }
            }
            Err(_) => {
                error!("Failed to get audio setting");
                self.output_volume = DEFAULT_OUTPUT_VOLUME;
            }
        }
        let i2s_driver_arc = self.i2s_driver.clone();
        let mut i2s_driver = i2s_driver_arc.lock().unwrap();
        i2s_driver.tx_enable().unwrap();
        i2s_driver.rx_enable().unwrap();

        self.enable_input(true).unwrap();
        self.enable_output(true).unwrap();
        info!("Audio codec started");
    }

    fn read_audio_data(&mut self, mut buffer: &mut Vec<u8>) -> Result<usize, Error> {
        let i2s_driver_arc = self.i2s_driver.clone();
        let mut i2s_driver = i2s_driver_arc.lock().unwrap();
        let bytes_read = i2s_driver.read(&mut buffer, 50)?;
        return Ok(bytes_read);
    }

    fn output_data(&mut self, audio_data: &[u8]) -> Result<(), Error> {
        const CHUNK_SIZE: usize = 4096;
        let i2s_driver = self.i2s_driver.clone();
        for chunk in audio_data.chunks(CHUNK_SIZE) {
            // 4. 逐块写入I2S驱动
            match i2s_driver.lock().unwrap().write(chunk, BLOCK) {
                Ok(bytes_written) => {
                    // 打印一些进度信息，方便调试
                    // info!("Successfully wrote {} bytes to I2S.", bytes_written);
                }
                Err(e) => {
                    // 如果在写入过程中出错，打印错误并跳出循环
                    info!("I2S write error on a chunk: {:?}", e);
                    break;
                }
            }
        }
        Ok(())
    }

    fn input_reference(&self) -> bool {
        return self.input_reference;
    }

    fn input_channels(&self) -> i32 {
        self.input_channels
    }

    fn test_play_pcm(&mut self, data: &[u8]) -> Result<(), Error> {
        const CHUNK_SIZE: usize = 4096;
        for chunk in data.chunks(CHUNK_SIZE) {
            // 4. 逐块写入I2S驱动
            match self.i2s_driver.lock().unwrap().write(chunk, BLOCK) {
                Ok(bytes_written) => {
                    // 打印一些进度信息，方便调试
                    // info!("Successfully wrote {} bytes to I2S.", bytes_written);
                }
                Err(e) => {
                    // 如果在写入过程中出错，打印错误并跳出循环
                    info!("I2S write error on a chunk: {:?}", e);
                    break;
                }
            }
        }
        Ok(())
    }

    fn play_opus(
        &mut self,
        opus_decoder: Arc<Mutex<OpusAudioDecoder>>,
        data: &[u8],
        pcm_buffer: &mut Vec<i16>,
    ) -> Result<(), Error> {
        let sample_rate = 16000; //# 采样率固定为16000Hz
        let channels = 2; //# 双声道

        // let mut opus_decoder = OpusAudioDecoder::new(sample_rate, channels).unwrap();

        let decode_result = opus_decoder.lock().unwrap().decode(&data);

        // let mut decoder = Box::new(OpusAudioDecoder::new(sample_rate, channels).unwrap());
        // let decode_result = decoder.decode(&opus_data);

        match decode_result {
            Ok(pcm_data) => {
                // info!("decode success.");
                let is_stereo = channels == 2;

                if !is_stereo {
                    //因为 p3文件是单声道的，而我们的 I2S 配置是双声道的，所以需要将单声道数据转换成双声道数据。
                    let pcm_mono_data_len = pcm_data.len();
                    // 1. 清空旧数据，但保留容量（不释放内存）
                    pcm_buffer.clear();
                    pcm_buffer.resize(pcm_mono_data_len * 2, 0);
                    // let mut pcm_stereo_buffer = vec![0i16; pcm_mono_data_len * 2];

                    // 2. 遍历单声道样本，并复制到立体声缓冲区的左右声道
                    for i in 0..pcm_mono_data_len {
                        let sample = pcm_data[i];
                        pcm_buffer[i * 2] = sample; // 左声道
                        pcm_buffer[i * 2 + 1] = sample; // 右声道
                    }

                    let pcm_stereo_bytes: &[u8] = unsafe {
                        core::slice::from_raw_parts(
                            pcm_buffer.as_ptr() as *const u8,
                            pcm_buffer.len() * std::mem::size_of::<i16>(),
                        )
                    };
                    self.test_play_pcm(pcm_stereo_bytes).unwrap();
                } else {
                    let pcm_stereo_bytes: &[u8] = unsafe {
                        core::slice::from_raw_parts(
                            pcm_data.as_ptr() as *const u8,
                            pcm_data.len() * std::mem::size_of::<i16>(),
                        )
                    };
                    self.test_play_pcm(pcm_stereo_bytes).unwrap();
                }
            }
            Err(e) => {
                info!("Opus decode error: {:?}", e);
                return Err(e);
            }
        }
        Ok(())
    }
}
