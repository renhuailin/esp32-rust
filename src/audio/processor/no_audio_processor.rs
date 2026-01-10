use crate::audio::processor::audio_processor::AudioProcessor;

pub struct NoAudioProcessor {}

impl NoAudioProcessor {
    pub fn new() -> Self {
        Self {}
    }
}

impl AudioProcessor for NoAudioProcessor {
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

    fn on_output(&mut self, callback: impl FnMut(Vec<i16>) + Send + 'static) {
        todo!()
    }

    fn on_vad_state_change(&mut self, callback: impl FnMut(bool) + Send + 'static) {
        todo!()
    }

    fn get_feed_size(&self) -> usize {
        todo!()
    }

    fn enable_device_aec(&mut self, enable: bool) {
        todo!()
    }
}
