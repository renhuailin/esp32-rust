use anyhow::{Error, Result};

use crate::audio::AudioStreamPacket;
pub trait Protocol {
    fn send_text(&mut self, text: &str) -> Result<()>;
    fn send_audio(&mut self, audio: &AudioStreamPacket) -> Result<()>;
    fn open_audio_channel(&mut self) -> Result<(), Error>;
}
