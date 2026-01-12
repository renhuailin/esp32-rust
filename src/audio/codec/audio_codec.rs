use anyhow::{Error, Result};

pub trait AudioCodec: Send {
    fn set_output_volume(&mut self, volume: u8) -> Result<(), Error>;
    fn enable_input(&mut self, enable: bool) -> Result<(), Error>;
    fn enable_output(&mut self, enable: bool) -> Result<(), Error>;

    fn input_enabled(&self) -> bool;
    fn output_enabled(&self) -> bool;

    fn start(&mut self);

    fn read_audio_data(&mut self, buffer: &mut Vec<u8>) -> Result<usize, Error>;
}
