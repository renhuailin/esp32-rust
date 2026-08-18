use std::sync::{Arc, Mutex};

use crate::{
    audio::codec::{
        audio_codec::AudioCodec,
        es7210::es7210::Es7210,
        es8311::{
            volume_manager::{self, load_volume_from_nvs, save_volume_to_nvs},
            Es8311,
        },
        make_channel_mask,
        opus::decoder::OpusAudioDecoder,
        types::CodecSampleInfo,
    },
    setting::nvs_setting::NvsSetting,
};
use anyhow::{Error, Ok, Result};
use esp_idf_hal::{
    delay::Delay,
    i2c::I2cDriver,
    i2s::{I2sBiDir, I2sDriver},
};
use log::{error, info};

type I2cProxy = shared_bus::I2cProxy<'static, Mutex<I2cDriver<'static>>>;
const DEFAULT_OUTPUT_VOLUME: u8 = 30;

/// I2S TX 写超时（ticks）。
/// 正常写入 4096B @16kHz/16bit/stereo（≈64ms 音频）远用不了这么久；
/// 超时说明 TX DMA 已停摆，必须放弃写入——否则会连锁死锁：
/// audio_loop 持 codec 锁卡死 → 主循环 play_silence 拿不到锁 → 整机无响应
/// （症状：持续嗒嗒响 + 按键失灵 + websocket 仍在收包）。
const fn ms_to_ticks(ms: u32) -> u32 {
    let t = ms * esp_idf_sys::configTICK_RATE_HZ / 1000;
    if t < 1 {
        1
    } else {
        t
    }
}

/// I2S TX 写超时（ticks）。
/// 正常写入 4096B @16kHz/16bit/stereo（≈64ms 音频）远用不了这么久；
/// 超时说明 TX DMA 已停摆，必须放弃写入——否则会连锁死锁：
/// audio_loop 持 codec 锁卡死 → 主循环 play_silence 拿不到锁 → 整机无响应
/// （症状：持续嗒嗒响 + 按键失灵 + websocket 仍在收包）。
const I2S_WRITE_TIMEOUT_TICKS: u32 = ms_to_ticks(250);
/// I2S RX 读超时。原值 1000 ticks 在 100Hz tick 下是 10 秒，
/// RX 卡死会让 audio_loop 持 codec 锁 10 秒，播放随之断流。
const I2S_READ_TIMEOUT_TICKS: u32 = ms_to_ticks(500);
pub struct XiaozhiAudioCodec {
    input_codec: Es7210<I2cProxy>,
    output_codec: Es8311<I2cProxy>,
    input_enabled: bool,
    output_enabled: bool,
    output_volume: u8,
    i2s_driver: Arc<Mutex<I2sDriver<'static, I2sBiDir>>>,
    // i2s_driver: Arc<Mutex<MixedI2sDriver>>,
    input_reference: bool,
    input_channels: i32,
}

impl XiaozhiAudioCodec {
    pub fn new(
        es8311_i2c_proxy: I2cProxy,
        es7210_i2c_proxy: I2cProxy,
        i2s_driver: I2sDriver<'static, I2sBiDir>,
        // i2s_driver: MixedI2sDriver,
    ) -> Self {
        let mut es8311 = Es8311::new(es8311_i2c_proxy);
        let mut delay = Delay::new_default();
        match es8311.open(&mut delay) {
            Result::Ok(_) => {
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
            Result::Ok(_) => {
                println!("初始化es7210成功");
            }
            Err(e) => {
                error!("初始化es7210失败:{:?}", e);
                // return Err(anyhow!("初始化es7210失败:{:?}", e));
            }
        }

        let input_reference = true;

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
        // info!("save 输出音量 to nvs: {}", volume);
        save_volume_to_nvs(volume)?;
        // info!("设置输出音量: {}", volume);
        self.output_codec.set_voice_volume(volume)?;
        Ok(())
    }

    fn get_output_volume(&self) -> Result<u8, anyhow::Error> {
        if let Result::Ok(volume) = load_volume_from_nvs() {
            Ok(volume)
        } else {
            Ok(volume_manager::DEFAULT_OUTPUT_VOLUME)
        }
    }

    fn enable_input(&mut self, enable: bool) -> Result<(), anyhow::Error> {
        if enable == self.input_enabled {
            return Ok(());
        }

        if enable {
            let mut fs = CodecSampleInfo {
                bits_per_sample: 16,
                channel: 4,
                channel_mask: make_channel_mask(0) as u16,
                sample_rate: 16000,
                mclk_multiple: 0,
            };
            if self.input_reference {
                fs.channel_mask |= make_channel_mask(1) as u16;
            }
            // self.input_codec.set_fs(fs)?;

            self.input_codec.enable()?;
        } else {
            self.input_codec.disable()?;
        }
        self.input_enabled = enable;
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
        self.output_enabled = enable;
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
            Result::Ok(setting) => {
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
        let bytes_read = i2s_driver.read(&mut buffer, I2S_READ_TIMEOUT_TICKS)?;
        return Ok(bytes_read);
    }

    fn output_data(&mut self, audio_data: &[u8]) -> Result<(), Error> {
        const CHUNK_SIZE: usize = 4096;
        let i2s_driver = self.i2s_driver.clone();
        for chunk in audio_data.chunks(CHUNK_SIZE) {
            // 4. 逐块写入I2S驱动（限时，见 I2S_WRITE_TIMEOUT_TICKS 注释）
            match i2s_driver.lock().unwrap().write(chunk, I2S_WRITE_TIMEOUT_TICKS) {
                Result::Ok(_bytes_written) => {}
                Err(e) => {
                    // TX 停摆：丢弃本包剩余数据，尽快释放 codec 锁，避免整机死锁
                    error!("I2S TX stalled, dropping rest of audio data: {:?}", e);
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
            match self.i2s_driver.lock().unwrap().write(chunk, I2S_WRITE_TIMEOUT_TICKS) {
                Result::Ok(_bytes_written) => {}
                Err(e) => {
                    error!("I2S TX stalled (test_play_pcm), dropping rest: {:?}", e);
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
            Result::Ok(pcm_data) => {
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
