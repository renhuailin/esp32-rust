use anyhow::Result;
use esp_idf_sys::es32_component_opus::{
    opus_decode, opus_decoder_create, opus_decoder_destroy, OpusDecoder,
};
use log::info;
pub struct OpusAudioDecoder {
    decoder: *mut OpusDecoder,
    sample_rate: i32,
}

impl OpusAudioDecoder {
    pub fn new(sample_rate: i32, channels: i32) -> Result<Self> {
        let error = std::ptr::null_mut();
        let decoder = unsafe { opus_decoder_create(sample_rate, channels, error) };

        if decoder.is_null() {
            return Err(anyhow::anyhow!(
                "Failed to create audio decoder, error code : {:?}",
                error
            ));
        }

        Ok(Self {
            decoder: decoder,
            sample_rate,
        })
    }

    pub fn decode(&mut self, opus_packet_data: &[u8]) -> Result<Vec<i16>, anyhow::Error> {
        // 1. 决定并创建一个足够大的输出缓冲区
        //    Opus文档建议缓冲区大小至少能容纳120ms的音频，以处理最坏情况。因为我们使用的是小智的 p3音频文件
        // 我们知道 每帧存储了 60ms 的音频数据，采样频率是 16000Hz,bit per sample = 16bit,为什么是 16bit,我在开发日志里有记载。
        //    对于48kHz采样率，120ms = 0.12s * 48000 samples/s = 5760 samples
        //     对于16kHz采样率，60ms = 0.06s * 16000 samples/s = 960 samples
        //    为了安全，我们创建一个稍大一些的Vec。
        let max_frame_size: usize = (60 * self.sample_rate / 1000) as usize;
        info!("max_frame_size: {}", max_frame_size);

        const CHANNELS: usize = 1; // 假设是单声道
        let mut pcm_buffer: Vec<i16> = vec![0; max_frame_size * CHANNELS];

        // 2. 准备其他参数

        let opus_packet_len = opus_packet_data.len() as i32;
        // let mut decoded_samples = 0;
        let decoded_samples = unsafe {
            opus_decode(
                self.decoder,
                opus_packet_data.as_ptr(),
                opus_packet_len,
                pcm_buffer.as_mut_ptr(),
                max_frame_size as i32,
                0,
            )
        };

        if decoded_samples < 0 {
            return Err(anyhow::anyhow!(
                "Failed to decode audio, error code: {:?}",
                decoded_samples
            ));
        } else {
            return Ok(pcm_buffer);
        }
    }
}

impl Drop for OpusAudioDecoder {
    fn drop(&mut self) {
        println!("Destroying opus audio decoder");
        unsafe {
            opus_decoder_destroy(self.decoder);
        };
    }
}
