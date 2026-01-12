use crate::audio::processor::audio_processor::AudioProcessor;

pub struct AfeAudioProcessor {}

impl AfeAudioProcessor {
    pub fn new() -> Self {
        Self {}
    }
}

impl AudioProcessor for AfeAudioProcessor {
    fn initialize(&mut self) {
        todo!()
    }

    fn feed(&mut self, data: &[i16]) {
        todo!()
    }

    fn start(&mut self) {
        todo!()
    }

    fn stop(&mut self) {
        todo!()
    }

    fn is_running(&self) -> bool {
        todo!()
    }

    fn get_feed_size(&self) -> usize {
        todo!()
    }

    fn enable_device_aec(&mut self, enable: bool) {
        todo!()
    }

    fn on_output(&mut self, callback: Box<dyn FnMut(Vec<i16>) + Send + 'static>) {
        todo!()
    }

    fn on_vad_state_change(&mut self, callback: Box<dyn FnMut(bool) + Send + 'static>) {
        todo!()
    }
}
