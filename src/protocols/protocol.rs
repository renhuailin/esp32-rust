use anyhow::{Error, Result};

use crate::{
    audio::codec::AudioStreamPacket,
    common::enums::{AbortReason, ListeningMode},
};
pub trait Protocol {
    fn send_text(&mut self, text: &str) -> Result<()>;
    fn send_audio(&mut self, audio: &AudioStreamPacket) -> Result<()>;
    fn open_audio_channel(&mut self) -> Result<bool, Error>;
    fn close_audio_channel(&mut self) -> Result<(), Error>;

    fn on_incoming_text<F>(&mut self, handler: F) -> Result<(), Error>
    where
        F: FnMut(&str) -> Result<(), Error> + Send + 'static;
    fn on_incoming_audio<F>(&mut self, handler: F) -> Result<(), Error>
    where
        F: FnMut(&AudioStreamPacket) -> Result<(), Error> + Send + 'static;

    fn is_timeout(&self) -> bool;

    fn is_audio_channel_opened(&self) -> bool;

    fn send_abort_speaking(&mut self, reason: AbortReason) -> Result<(), Error>;

    fn send_start_linstening(&mut self, listening_mode: ListeningMode) -> Result<(), Error>;
}
