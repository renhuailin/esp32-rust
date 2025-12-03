use anyhow::{anyhow, Result};
use esp_idf_sys::es32_component_opus::{
    opus_encode, opus_encoder_create, opus_encoder_destroy, OpusEncoder, OPUS_APPLICATION_VOIP,
};
use log::{error, info};

const MAX_OPUS_PACKET_SIZE: usize = 3000;
pub struct OpusAudioEncoder {
    encoder: *mut OpusEncoder,
    // sample_rate: i32,
    // duration_ms: i32,
    frame_size: usize,
    in_buffer: Vec<i16>,
}

impl OpusAudioEncoder {
    /// 创建一个新的 Opus 音频编码器实例
    ///
    /// # 参数
    /// * `sample_rate` - 音频采样率（例如 16000 表示 16kHz）
    /// * `channels` - 声道数（1 表示单声道，2 表示立体声）
    /// * `duration_ms` - 音频帧持续时间（毫秒）
    ///
    /// # 返回值
    /// 返回一个新的 OpusAudioEncoder 实例
    pub fn new(sample_rate: i32, channels: i32, duration_ms: i32) -> Result<Self> {
        // let error = std::ptr::null_mut();
        let mut error: i32 = 0;
        let encoder = unsafe {
            opus_encoder_create(
                sample_rate,
                channels,
                OPUS_APPLICATION_VOIP.try_into()?, // 音频内容主要是用于什么场景的,
                // 有三个值：OPUS_APPLICATION_VOIP，
                // OPUS_APPLICATION_AUDIO，
                // OPUS_APPLICATION_RESTRICTED_LOWDELAY
                &mut error,
            )
        };

        if error != 0 || encoder.is_null() {
            return Err(anyhow::anyhow!(
                "Failed to create opus audio encoder, error code : {:?}",
                error
            ));
        }

        let frame_size = sample_rate / 1000 * channels * duration_ms;

        Ok(Self {
            encoder,
            // sample_rate,
            // duration_ms,
            frame_size: frame_size.try_into()?,
            in_buffer: Vec::new(),
        })
    }

    // pub fn encode(&mut self, pcm_packet_data: &[u8]) -> Result<Vec<i16>, anyhow::Error> {
    //     Ok(bits::decode_bits(opus_packet_data))
    // }

    pub fn encode<F>(&mut self, mut pcm: Vec<i16>, handler: &mut F) -> Result<()>
    where
        F: FnMut(Vec<u8>),
    {
        // MutexGuard 的作用域会自动处理锁的释放，比 C++ 的 std::lock_guard 更安全
        // 注意：如果这个方法本身就在一个 Mutex 保护下，这里就不需要额外的锁了

        // let audio_enc = self.audio_enc.ok_or_else(|| {
        //     error!("Audio encoder is not configured");
        //     anyhow!("Audio encoder is not configured")
        // })?;

        info!("add pcm to buffer...");

        // --- 这里是 `in_buffer_` 逻辑的 Rust 实现 ---
        // 直接使用 `append`，它会移动 pcm 的所有元素，并在 pcm 变空后返回。
        // 这同时处理了 in_buffer 为空和不为空两种情况，更简洁。
        self.in_buffer.append(&mut pcm);

        info!(
            "in_buffer.len= {}, self.frame_size = {}",
            self.in_buffer.len(),
            self.frame_size
        );

        // --- 编码循环 ---
        // 只要缓冲区里的数据足够一个帧，就继续编码
        while self.in_buffer.len() >= self.frame_size {
            info!("开始编码pcm...");

            // 创建一个足够大的输出缓冲区
            // 使用 Vec<u8> 并设置容量，比栈上的 C-style 数组更安全
            let mut opus_out = Vec::with_capacity(MAX_OPUS_PACKET_SIZE);
            // opus FFI 需要一个裸指针和最大长度
            let opus_ptr = opus_out.as_mut_ptr();
            let max_len = opus_out.capacity() as i32;

            if self.encoder.is_null() {
                return Err(anyhow!("Audio encoder is not configured"));
            }
            info!("开始调用unsafe opus_encode");
            let data: &[i16] = &self.in_buffer[..self.frame_size];
            let ret = unsafe {
                // 调用 FFI 函数
                opus_encode(
                    self.encoder,
                    // self.in_buffer.as_ptr(), // 输入 PCM 数据的裸指针
                    data.as_ptr(), // 输入 PCM 数据的裸指针
                    // self.frame_size as i32,
                    (data.len() / 2) as i32,
                    // 960,
                    opus_ptr,
                    MAX_OPUS_PACKET_SIZE as i32,
                )
            };
            info!("调用unsafe opus_encode 结束");
            if ret < 0 {
                let err_msg = format!("Failed to encode audio, error code: {}", ret);
                error!("{}", err_msg);
                return Err(anyhow!(err_msg));
            }

            // `ret` 是实际编码后的字节数
            let encoded_len = ret as usize;

            // 安全地设置 Vec 的实际长度
            unsafe {
                opus_out.set_len(encoded_len);
            }

            // 调用 handler 闭包，将编码后的数据的所有权移交给它
            info!("调用 handler 闭包，将编码后的数据的所有权移交给它");
            handler(opus_out);

            // --- 从输入缓冲区移除已处理的数据 ---
            self.in_buffer.drain(0..self.frame_size);

            // drain() 会返回一个迭代器，更高效的方式是直接操作底层数据
            // 但 drain 对于 Vec 来说已经很高效了
            // 另一种高效的方法是 `Vec::remove` in a loop, but `drain` is more idiomatic.
            // A potentially faster but more complex way:
            // let remaining_len = self.in_buffer.len() - self.frame_size;
            // self.in_buffer.copy_within(self.frame_size.., 0);
            // self.in_buffer.truncate(remaining_len);
        }

        Ok(())
    }
}

impl Drop for OpusAudioEncoder {
    fn drop(&mut self) {
        if !self.encoder.is_null() {
            unsafe {
                opus_encoder_destroy(self.encoder);
            }
        }
    }
}
