use anyhow::Error;

pub trait AudioCodec {
    fn set_output_volume(&mut self, volume: u8) -> Result<(), Error>;
    fn enable_input(&mut self, enable: bool) -> Result<(), Error>;
    fn enable_output(&mut self, enable: bool) -> Result<(), Error>;
}
