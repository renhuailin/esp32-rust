#[derive(Clone, Debug)]
pub struct CodecSampleInfo {
    pub bits_per_sample: u8,
    pub channel: u8,
    pub channel_mask: u16,
    pub sample_rate: u32,
    pub mclk_multiple: u8,
}

#[derive(Clone, Debug)]
pub struct AudioStreamPacket {
    pub sample_rate: i32,
    pub frame_duration: i32,
    pub timestamp: u32,
    // std::vector<uint8_t> payload;
    pub payload: Vec<u8>,
}
