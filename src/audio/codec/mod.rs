pub mod audio_codec;
pub mod es7210;
pub mod es8311;
pub mod opus;
pub mod xiaozhi_audio_codec;

pub const AUDIO_INPUT_SAMPLE_RATE: u32 = 24000;
pub const AUDIO_OUTPUT_SAMPLE_RATE: u32 = 24000;
pub const I2S_MCLK_MULTIPLE_256: i32 = 256;

pub const OPUS_FRAME_DURATION_MS: usize = 60;
pub const MAX_AUDIO_PACKETS_IN_QUEUE: usize = 2400 / OPUS_FRAME_DURATION_MS;

#[derive(Clone, Debug)]
pub struct AudioStreamPacket {
    pub sample_rate: i32,
    pub frame_duration: i32,
    pub timestamp: u32,
    // std::vector<uint8_t> payload;
    pub payload: Vec<u8>,
}
