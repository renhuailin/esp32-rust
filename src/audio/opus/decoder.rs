use anyhow::Result;
use esp_idf_sys::es32_component_opus::{opus_decoder_create, opus_decoder_destroy, OpusDecoder};
pub struct OpusAudioDecoder {
    decoder: Option<*mut OpusDecoder>,
}

impl OpusAudioDecoder {
    pub fn new(sample_rate: i32, channels: i32) -> Result<Self> {
        let error = std::ptr::null_mut();
        let decoder = unsafe { opus_decoder_create(sample_rate, channels, error) };

        if decoder.is_null() {
            return Err(anyhow::anyhow!(
                "Failed to create audio decoder, error code: {:?}",
                error
            ));
        }

        Ok(Self {
            decoder: Some(decoder),
        })
    }
}

impl Drop for OpusAudioDecoder {
    fn drop(&mut self) {
        if let Some(decoder) = self.decoder {
            println!("Destroying opus audio decoder");
            unsafe {
                opus_decoder_destroy(decoder);
            };
        }
    }
}
