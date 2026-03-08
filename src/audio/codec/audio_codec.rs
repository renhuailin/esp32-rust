use std::sync::{Arc, Mutex};

use anyhow::{Error, Result};

use crate::audio::codec::opus::decoder::OpusAudioDecoder;

pub trait AudioCodec: Send {
    fn set_output_volume(&mut self, volume: u8) -> Result<(), Error>;
    fn enable_input(&mut self, enable: bool) -> Result<(), Error>;
    fn enable_output(&mut self, enable: bool) -> Result<(), Error>;

    fn input_enabled(&self) -> bool;
    fn output_enabled(&self) -> bool;
    fn input_reference(&self) -> bool;

    fn input_channels(&self) -> i32;

    fn start(&mut self);

    fn read_audio_data(&mut self, buffer: &mut Vec<u8>) -> Result<usize, Error>;

    fn output_data(&mut self, data: &[u8]) -> Result<(), Error>;

    fn test_play_pcm(&mut self, data: &[u8]) -> Result<(), Error>;

    fn play_opus(
        &mut self,
        opus_decoder: Arc<Mutex<OpusAudioDecoder>>,
        data: &[u8],
        pcm_buffer: &mut Vec<i16>,
    ) -> Result<(), Error>;
}
