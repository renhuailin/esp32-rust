use crate::audio::processor::audio_processor::AudioProcessor;

pub struct NoAudioProcessor {
    input_sample_rate: u32,
    audio_output_callback: Option<Box<dyn FnMut(Vec<i16>) + Send + 'static>>,
}

impl NoAudioProcessor {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            input_sample_rate: sample_rate,
            audio_output_callback: None,
        }
    }
}

impl AudioProcessor for NoAudioProcessor {
    fn initialize(&mut self) {
        todo!()
    }

    fn feed(&mut self, data: &[i16]) {
        if let Some(callback) = &mut self.audio_output_callback {
            callback(data.to_vec());
        }
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
        return (30 * self.input_sample_rate / 1000).try_into().unwrap();
    }

    fn enable_device_aec(&mut self, enable: bool) {
        todo!()
    }

    fn on_output(&mut self, callback: Box<dyn FnMut(Vec<i16>) + Send + 'static>) {
        self.audio_output_callback = Some(callback);
    }

    fn on_vad_state_change(&mut self, callback: Box<dyn FnMut(bool) + Send + 'static>) {
        todo!()
    }
}
