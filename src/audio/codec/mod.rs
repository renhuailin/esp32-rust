pub mod audio_codec;
pub mod es7210;
pub mod es8311;
pub mod opus;
pub mod types;
pub mod xiaozhi_audio_codec;

pub const AUDIO_INPUT_SAMPLE_RATE: u32 = 16000;
pub const AUDIO_OUTPUT_SAMPLE_RATE: u32 = 16000;
pub const I2S_MCLK_MULTIPLE_256: i32 = 256;

pub const OPUS_FRAME_DURATION_MS: usize = 60;
pub const MAX_AUDIO_PACKETS_IN_QUEUE: usize = 2400 / OPUS_FRAME_DURATION_MS;

pub fn make_channel_mask(channel: u8) -> u8 {
    1 << channel
}
