use anyhow::{Error, Result};
use esp_idf_svc::handle;

use crate::{audio::AudioStreamPacket, common::enums::AbortReason};
pub trait Protocol {
    fn send_text(&mut self, text: &str) -> Result<()>;
    fn send_audio(&mut self, audio: &AudioStreamPacket) -> Result<()>;
    fn open_audio_channel(&mut self) -> Result<bool, Error>;

    fn on_incoming_text<F>(&mut self, handler: F) -> Result<(), Error>
    where
        F: FnMut(&str) -> Result<(), Error> + Send + 'static;
    fn on_incoming_audio<F>(&mut self, handler: F) -> Result<(), Error>
    where
        F: FnMut(&AudioStreamPacket) -> Result<(), Error> + Send + 'static;

    fn is_timeout(&self) -> bool;

    fn is_audio_channel_opened(&self) -> bool;

    fn send_abort_speaking(&mut self, reason: AbortReason) -> Result<(), Error>;
}
