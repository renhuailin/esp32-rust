use std::collections::VecDeque;
use std::ffi::{c_void, CStr, CString};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use esp_idf_sys::es32_component_esp_sr::{
    aec_mode_t_AEC_MODE_SR_HIGH_PERF, afe_config_init,
    afe_memory_alloc_mode_t_AFE_MEMORY_ALLOC_MORE_PSRAM, afe_mode_t_AFE_MODE_HIGH_PERF,
    afe_type_t_AFE_TYPE_SR, esp_afe_handle_from_config, esp_afe_sr_data_t, esp_afe_sr_iface_t,
    esp_srmodel_init, ESP_WN_PREFIX,
};
use esp_idf_sys::ESP_FAIL;
use log::{error, info, warn};

use crate::audio::codec::opus::encoder::OpusAudioEncoder;
use crate::utils::ffi::c_task_trampoline;

/// 唤醒词检测回调
type WakeWordCallback = Box<dyn FnMut(String) + Send + 'static>;

/// 内部共享状态
struct WakeWordState {
    /// 是否正在检测
    is_detecting: bool,
    /// 唤醒词检测回调
    wake_word_callback: Option<WakeWordCallback>,
    /// 唤醒词PCM数据缓冲（保留约2秒）
    wake_word_pcm: VecDeque<Vec<i16>>,
    /// 编码后的Opus数据队列
    wake_word_opus: VecDeque<Vec<u8>>,
    /// 最近检测到的唤醒词
    last_detected_wake_word: String,
}

pub struct WakeWordService {
    afe_data: SendPtr<esp_afe_sr_data_t>,
    afe_iface: SendPtr<esp_afe_sr_iface_t>,

    /// 输入通道数
    input_channels: usize,
    /// 支持的唤醒词列表
    wake_words: Vec<String>,

    /// 共享状态 + 条件变量
    state: Arc<Mutex<WakeWordState>>,
    cond: Arc<Condvar>,
}

#[derive(Clone, Copy)]
struct SendPtr<T>(*mut T);
unsafe impl<T> Send for SendPtr<T> {}
unsafe impl<T> Sync for SendPtr<T> {}

impl<T> SendPtr<T> {
    fn as_ptr(&self) -> *mut T {
        self.0
    }
}

impl WakeWordService {
    pub fn new(input_channels: usize, input_reference: bool) -> Result<Self, anyhow::Error> {
        info!("Initializing WakeWordService (AFE_TYPE_SR)");

        let ref_num = if input_reference { 1 } else { 0 };
        info!(
            "input_channels: {}, input_reference: {}, ref_num: {}",
            input_channels, input_reference, ref_num
        );

        // 构建 input_format: 主声道用 'M'，参考声道用 'R'
        let mut input_format = String::new();
        for _ in 0..(input_channels - ref_num) {
            input_format.push('M');
        }
        for _ in 0..ref_num {
            input_format.push('R');
        }
        info!("AFE Input Format: {}", input_format);

        // 初始化 SR 模型列表
        let model_c_str = CString::new("model").unwrap();
        let models = unsafe { esp_srmodel_init(model_c_str.as_ptr() as *const u8) };

        if models.is_null() {
            error!("Failed to initialize wakenet model!");
            return Err(anyhow::anyhow!("Failed to initialize wakenet model"));
        }

        // 遍历模型，查找 wakenet 模型并提取唤醒词
        let mut wake_words: Vec<String> = Vec::new();
        unsafe {
            let count = (*models).num as usize;
            info!("Loaded {} models:", count);

            for i in 0..count {
                let name_ptr = *((*models).model_name.add(i));
                if !name_ptr.is_null() {
                    let name = CStr::from_ptr(name_ptr).to_string_lossy();
                    info!("  Model {}: {}", i, name);

                    // 查找 wakenet 模型
                    let wn_prefix = CStr::from_ptr(ESP_WN_PREFIX.as_ptr() as *const u8);
                    if name.contains(wn_prefix.to_str().unwrap_or("")) {
                        info!("  Found wakenet model: {}", name);

                        // 获取唤醒词
                        let wake_words_str =
                            esp_idf_sys::es32_component_esp_sr::esp_srmodel_get_wake_words(
                                models, name_ptr,
                            );
                        if !wake_words_str.is_null() {
                            let ww_cstr = CStr::from_ptr(wake_words_str as *const u8);
                            let ww_str = ww_cstr.to_string_lossy().to_string();
                            // 分号分割多个唤醒词
                            for word in ww_str.split(';') {
                                if !word.is_empty() {
                                    wake_words.push(word.to_string());
                                }
                            }
                            info!("  Wake words: {:?}", wake_words);
                        }
                    }
                }
            }
        }

        if wake_words.is_empty() {
            error!("No wakenet model found!");
        }

        // 创建 AFE 配置 — 使用 AFE_TYPE_SR (语音识别模式)
        let input_format_c_str = CString::new(input_format).unwrap();
        let afe_config = unsafe {
            afe_config_init(
                input_format_c_str.as_ptr(),
                models, // 传入 models，SR 模式需要
                afe_type_t_AFE_TYPE_SR,
                afe_mode_t_AFE_MODE_HIGH_PERF,
            )
        };

        // AEC 配置
        unsafe {
            (*afe_config).aec_init = input_reference;
            (*afe_config).aec_mode = aec_mode_t_AEC_MODE_SR_HIGH_PERF;
            (*afe_config).afe_perferred_core = 1;
            (*afe_config).afe_perferred_priority = 1;
            (*afe_config).memory_alloc_mode = afe_memory_alloc_mode_t_AFE_MEMORY_ALLOC_MORE_PSRAM;
        }

        // 创建 AFE 实例
        let afe_iface: *mut esp_afe_sr_iface_t = unsafe { esp_afe_handle_from_config(afe_config) };
        let create_from_config = unsafe { (*afe_iface).create_from_config.unwrap() };
        let afe_data: *mut esp_afe_sr_data_t = unsafe { create_from_config(afe_config) };

        // 共享状态
        let state = Arc::new(Mutex::new(WakeWordState {
            is_detecting: false,
            wake_word_callback: None,
            wake_word_pcm: VecDeque::new(),
            wake_word_opus: VecDeque::new(),
            last_detected_wake_word: String::new(),
        }));
        let cond = Arc::new(Condvar::new());

        let mut service = Self {
            afe_data: SendPtr(afe_data),
            afe_iface: SendPtr(afe_iface as *mut _),
            input_channels,
            wake_words,
            state: state.clone(),
            cond: cond.clone(),
        };

        // 启动音频检测任务
        service.audio_detection_task();
        info!("WakeWordService initialized");
        Ok(service)
    }

    /// 注册唤醒词检测回调
    pub fn on_wake_word_detected(&mut self, callback: WakeWordCallback) {
        // 注意：这里是注册回调，不是检测到唤醒词！真实唤醒有单独的日志。
        info!("on_wake_word_detected:: 注册唤醒词回调");
        let mut state = self.state.lock().unwrap();
        state.wake_word_callback = Some(callback);
    }

    /// 开始检测
    pub fn start_detection(&mut self) {
        // 每次启动检测前复位 AFE 缓冲（清掉 ringbuf 里残留的旧音频），
        // 对齐开机路径已验证的行为。
        // 注意：不要在这里调用 disable/enable_wakenet 或 disable/enable_aec！
        // 实测这两个模块复位 API 在 esp-sr v2.1.4 上会把 wakenet 弄成
        // 无法唤醒的状态（enable 后检测永久失效），属于禁用操作。
        unsafe {
            if !self.afe_data.as_ptr().is_null() {
                ((*self.afe_iface.as_ptr()).reset_buffer.unwrap())(self.afe_data.as_ptr());
                info!("AFE buffer reset (start_detection)");
            }
        }
        let mut state = self.state.lock().unwrap();
        if !state.is_detecting {
            state.is_detecting = true;
            info!("Wake word detection started");
        }
        drop(state);
        self.cond.notify_all();
    }

    /// 停止检测
    pub fn stop_detection(&mut self) {
        {
            let mut state = self.state.lock().unwrap();
            state.is_detecting = false;
        }
        // 重置 AFE 缓冲区
        unsafe {
            if !self.afe_data.as_ptr().is_null() {
                ((*self.afe_iface.as_ptr()).reset_buffer.unwrap())(self.afe_data.as_ptr());
            }
        }
        info!("Wake word detection stopped");
    }

    /// 是否正在检测
    pub fn is_detection_running(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.is_detecting
    }

    /// 复位 AFE 输入/输出缓冲（不改变检测开关状态）。
    /// 开机首次启动检测前调用一次，与 Listening -> Idle 路径的 reset 行为对齐。
    pub fn reset_afe_buffer(&mut self) {
        unsafe {
            if !self.afe_data.as_ptr().is_null() {
                ((*self.afe_iface.as_ptr()).reset_buffer.unwrap())(self.afe_data.as_ptr());
                info!("AFE buffer reset");
            }
        }
    }

    /// 喂入音频数据
    pub fn feed(&mut self, data: &[i16]) {
        if data.is_empty() {
            return;
        }

        let iface_ptr = self.afe_iface.as_ptr();
        let data_ptr = self.afe_data.as_ptr();

        if iface_ptr.is_null() || data_ptr.is_null() {
            error!("Critical: AFE pointers are null!");
            return;
        }

        unsafe {
            if let Some(feed_func) = (*iface_ptr).feed {
                feed_func(data_ptr, data.as_ptr() as *const _);
            } else {
                error!("Critical: AFE feed function pointer is null!");
            }
        }
    }

    /// 获取 feed 大小
    pub fn get_feed_size(&self) -> usize {
        if self.afe_data.as_ptr().is_null() {
            return 0;
        }
        let feed_chunksize = unsafe {
            ((*self.afe_iface.as_ptr()).get_feed_chunksize.unwrap())(self.afe_data.as_ptr())
                as usize
        };
        feed_chunksize * self.input_channels
    }

    /// 当前 AFE 配置的输入声道数（喂料路径据此换算交错帧数）
    pub fn input_channels(&self) -> usize {
        self.input_channels
    }

    /// 获取支持的唤醒词列表
    pub fn wake_words(&self) -> &[String] {
        &self.wake_words
    }

    /// 获取最近检测到的唤醒词
    pub fn last_detected_wake_word(&self) -> String {
        let state = self.state.lock().unwrap();
        state.last_detected_wake_word.clone()
    }

    /// 存储唤醒词PCM数据（保留约2秒，检测周期30ms）
    fn store_wake_word_data(state: &mut WakeWordState, data: &[i16]) {
        state.wake_word_pcm.push_back(data.to_vec());
        // keep about 2 seconds of data, detect duration is 30ms
        while state.wake_word_pcm.len() > 2000 / 30 {
            state.wake_word_pcm.pop_front();
        }
    }

    /// 音频检测任务（对应 C++ 的 AudioDetectionTask）
    fn audio_detection_task(&mut self) {
        info!("Starting audio detection task");

        let state_clone = self.state.clone();
        let cond_clone = self.cond.clone();
        let afe_iface_wrapper = self.afe_iface;
        let afe_data_wrapper = self.afe_data;
        let wake_words = self.wake_words.clone();

        let task_closure: Box<dyn FnOnce() + Send> = Box::new(move || {
            let afe_iface = afe_iface_wrapper.as_ptr() as *const esp_afe_sr_iface_t;
            let afe_data = afe_data_wrapper.as_ptr();

            unsafe {
                let fetch_size = ((*afe_iface).get_fetch_chunksize.unwrap())(afe_data);
                let feed_size = ((*afe_iface).get_feed_chunksize.unwrap())(afe_data);
                info!(
                    "Audio detection task started, feed size: {} fetch size: {}",
                    feed_size, fetch_size
                );

                loop {
                    // 阶段1: 等待检测开始信号
                    let mut state_guard = state_clone.lock().unwrap();
                    while !state_guard.is_detecting {
                        state_guard = cond_clone.wait(state_guard).unwrap();
                    }
                    drop(state_guard);

                    // 阶段2: 获取 AFE 处理结果
                    // 使用有限超时（约 0.5~5 秒，取决于 FreeRTOS tick 频率）而非无限阻塞：
                    // 1. AFE 数据管道异常时能及时暴露（warn 日志）而不是无声卡死
                    // 2. stop_detection 之后任务能更快回到等待阶段，避免错过 start 信号
                    let res = ((*afe_iface).fetch_with_delay.unwrap())(afe_data, 500);
                    if res.is_null() || (*res).ret_value == ESP_FAIL {
                        warn!("AFE fetch no result in time, detection pipeline may be stalled");
                        continue;
                    }

                    // 阶段3: 处理结果
                    let mut state_guard = state_clone.lock().unwrap();

                    // 再次检查检测状态（防止在 fetch 期间被 stop）
                    if !state_guard.is_detecting {
                        continue;
                    }

                    // fetch 心跳探针：确认检测循环存活，并观察 AFE 输出内容。
                    // max_abs 长期为 0 => AFE 输出被静音（AEC 吞掉全部信号）或喂料全零；
                    // max_abs 正常但无唤醒 => wakenet 模型状态异常。
                    static FETCH_COUNT: AtomicU32 = AtomicU32::new(0);
                    let fetches = FETCH_COUNT.fetch_add(1, Ordering::Relaxed);
                    if fetches % 50 == 0 {
                        let probe_len = (*res).data_size as usize / std::mem::size_of::<i16>();
                        let probe_slice =
                            std::slice::from_raw_parts((*res).data as *const i16, probe_len);
                        let max_abs = probe_slice
                            .iter()
                            .map(|s| s.unsigned_abs())
                            .max()
                            .unwrap_or(0);
                        // info!(
                        //     "afe fetch alive: total {} fetches, wakeup_state={}, data_len={}, max_abs={}",
                        //     fetches + 1,
                        //     (*res).wakeup_state as i32,
                        //     probe_len,
                        //     max_abs
                        // );
                    }

                    // 存储唤醒词PCM数据
                    let data_len = (*res).data_size as usize / std::mem::size_of::<i16>();
                    let data_slice =
                        std::slice::from_raw_parts((*res).data as *const i16, data_len);
                    Self::store_wake_word_data(&mut state_guard, data_slice);

                    // 检查唤醒词检测
                    // WAKENET_DETECTED 对应 C++ 中的 res->wakeup_state == WAKENET_DETECTED
                    let wakeup_state = (*res).wakeup_state;
                    if wakeup_state
                        == esp_idf_sys::es32_component_esp_sr::wakenet_state_t_WAKENET_DETECTED
                    {
                        // 停止检测：对齐 C++ StopDetection()——清标志位后立即
                        // reset_buffer，把唤醒词音频从输入 ringbuf 中清掉。
                        // 否则这些残留数据会冻结整个会话期间，并在下一轮
                        // start_detection 后被 AFE 首先消费，干扰管道恢复。
                        state_guard.is_detecting = false;
                        ((*afe_iface).reset_buffer.unwrap())(afe_data);

                        // 获取唤醒词索引 (C++: res->wake_word_index - 1)
                        let wake_word_index = (*res).wake_word_index as usize;
                        if wake_word_index > 0 && wake_word_index <= wake_words.len() {
                            let wake_word = wake_words[wake_word_index - 1].clone();
                            state_guard.last_detected_wake_word = wake_word.clone();
                            info!("Wake word detected: {}", wake_word);

                            if let Some(ref mut cb) = state_guard.wake_word_callback {
                                cb(wake_word);
                            }
                        } else {
                            error!(
                                "Invalid wake word index: {} (wake_words len: {})",
                                wake_word_index,
                                wake_words.len()
                            );
                        }
                    }
                }
            }
        });

        let closure_box = Box::new(task_closure);
        let closure_ptr = Box::into_raw(closure_box);

        unsafe {
            esp_idf_sys::xTaskCreatePinnedToCore(
                Some(c_task_trampoline),
                b"audio_detection\0".as_ptr() as *const u8,
                16 * 1024,
                closure_ptr as *mut c_void,
                3,
                std::ptr::null_mut(),
                1,
            );
        }
    }

    /// 对应 C++ AfeWakeWord::EncodeWakeWordData：
    /// 把缓存的唤醒词 PCM（约 2 秒，AFE 处理后的单声道 16k 数据）编码为 opus 包
    pub fn encode_wake_word_data(&mut self) {
        {
            let mut state = self.state.lock().unwrap();
            state.wake_word_opus.clear();
            if state.wake_word_pcm.is_empty() {
                return;
            }
        }

        let start_time = std::time::Instant::now();
        // 16kHz 单声道 60ms 帧，与 C++ OpusEncoderWrapper(16000, 1, OPUS_FRAME_DURATION_MS) 一致
        let mut encoder = match OpusAudioEncoder::new(16000, 1, 60) {
            Ok(encoder) => encoder,
            Err(e) => {
                error!("Failed to create opus encoder for wake word: {:?}", e);
                return;
            }
        };
        encoder.set_complexity(0); // 0 is the fastest

        let mut state = self.state.lock().unwrap();
        let pcm_blocks: Vec<Vec<i16>> = state.wake_word_pcm.drain(..).collect();
        let opus_queue = &mut state.wake_word_opus;
        let mut packets: usize = 0;
        let mut callback = |opus: Vec<u8>| {
            opus_queue.push_back(opus);
            packets += 1;
        };
        for pcm in pcm_blocks {
            // encoder 内部按 60ms 帧组包，不足一帧的块会自动拼接，末尾残留丢弃
            if let Err(e) = encoder.encode(pcm, &mut callback) {
                error!("Failed to encode wake word pcm: {:?}", e);
            }
        }
        info!(
            "Encode wake word opus {} packets in {} ms",
            packets,
            start_time.elapsed().as_millis()
        );
    }

    /// 对应 C++ AfeWakeWord::GetWakeWordOpus：
    /// 取出一个编码后的 opus 包，None 表示已全部取出
    pub fn get_wake_word_opus(&mut self) -> Option<Vec<u8>> {
        self.state.lock().unwrap().wake_word_opus.pop_front()
    }
}

impl Drop for WakeWordService {
    fn drop(&mut self) {
        info!("WakeWordService destroyed");
        unsafe {
            if !self.afe_data.as_ptr().is_null() {
                ((*self.afe_iface.as_ptr()).destroy.unwrap())(self.afe_data.as_ptr());
            }
        }
    }
}
