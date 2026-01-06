use anyhow::{Error, Result};
use esp_idf_svc::handle;

use crate::audio::AudioStreamPacket;
pub trait Protocol {
    fn send_text(&mut self, text: &str) -> Result<()>;
    fn send_audio(&mut self, audio: &AudioStreamPacket) -> Result<()>;
    fn open_audio_channel(&mut self) -> Result<(), Error>;

    fn on_incoming_text<F>(&mut self, handler: F) -> Result<(), Error>
    where
        F: FnMut(&str) -> Result<(), Error> + Send + 'static;
    fn on_incoming_audio<F>(&mut self, handler: F) -> Result<(), Error>
    where
        F: FnMut(&AudioStreamPacket) -> Result<(), Error> + Send + 'static;
}
