use std::collections::VecDeque;
use std::ffi::{c_void, CStr, CString};
use std::sync::{Arc, Condvar, Mutex};

use esp_idf_sys::es32_component_esp_sr::{
    aec_mode_t_AEC_MODE_SR_HIGH_PERF, afe_config_init,
    afe_memory_alloc_mode_t_AFE_MEMORY_ALLOC_MORE_PSRAM, afe_mode_t_AFE_MODE_HIGH_PERF,
    afe_type_t_AFE_TYPE_SR, esp_afe_handle_from_config, esp_afe_sr_data_t, esp_afe_sr_iface_t,
    esp_srmodel_init, ESP_WN_PREFIX,
};
use esp_idf_sys::ESP_FAIL;
use log::{error, info};

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
        info!("检测到唤醒词!");
        let mut state = self.state.lock().unwrap();
        state.wake_word_callback = Some(callback);
    }

    /// 开始检测
    pub fn start_detection(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.is_detecting = true;
        self.cond.notify_all();
    }

    /// 停止检测
    pub fn stop_detection(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.is_detecting = false;
        // 重置 AFE 缓冲区
        unsafe {
            if !self.afe_data.as_ptr().is_null() {
                ((*self.afe_iface.as_ptr()).reset_buffer.unwrap())(self.afe_data.as_ptr());
            }
        }
    }

    /// 是否正在检测
    pub fn is_detection_running(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.is_detecting
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
                    let res = ((*afe_iface).fetch_with_delay.unwrap())(afe_data, u32::MAX);
                    if res.is_null() || (*res).ret_value == ESP_FAIL {
                        continue;
                    }

                    // 阶段3: 处理结果
                    let mut state_guard = state_clone.lock().unwrap();

                    // 再次检查检测状态（防止在 fetch 期间被 stop）
                    if !state_guard.is_detecting {
                        continue;
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
                        // 停止检测
                        state_guard.is_detecting = false;

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
