use std::ffi::CString;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use esp_idf_sys::es32_component_esp_sr::{
    aec_mode_t_AEC_MODE_VOIP_HIGH_PERF, afe_config_init,
    afe_memory_alloc_mode_t_AFE_MEMORY_ALLOC_MORE_PSRAM, afe_mode_t_AFE_MODE_HIGH_PERF,
    afe_ns_mode_t_AFE_NS_MODE_NET, afe_type_t_AFE_TYPE_VC, esp_afe_handle_from_config,
    esp_afe_sr_data_t, esp_afe_sr_iface_t, esp_srmodel_filter, esp_srmodel_init,
    vad_mode_t_VAD_MODE_0, vad_state_t_VAD_SILENCE, vad_state_t_VAD_SPEECH, ESP_NSNET_PREFIX,
    ESP_VADN_PREFIX,
};
use esp_idf_sys::ESP_FAIL;
use log::{error, info};

use crate::audio::{codec::audio_codec::AudioCodec, processor::audio_processor::AudioProcessor};

#[derive(Clone, Copy)]
struct SendPtr<T>(*mut T);

// 强制实现 Send 和 Sync，告诉编译器这些指针可以在线程间传递
// 前提：我们确信 ESP-SR 的这些 API 是线程安全的，或者我们通过 Mutex 保护了它们
unsafe impl<T> Send for SendPtr<T> {}
unsafe impl<T> Sync for SendPtr<T> {}

impl<T> SendPtr<T> {
    fn as_ptr(&self) -> *mut T {
        self.0
    }
}

// 定义回调类型
type OutputCallback = Box<dyn FnMut(Vec<i16>) + Send + 'static>;
type VadCallback = Box<dyn FnMut(bool) + Send + 'static>;

// 内部状态，需要在线程间共享
struct ProcessorState {
    is_running: bool,
    is_speaking: bool,
    // 回调函数存放在这里
    output_callback: Option<OutputCallback>,
    vad_callback: Option<VadCallback>,
}

pub struct AfeAudioProcessor {
    codec: Arc<Mutex<dyn AudioCodec + 'static>>,
    // afe_data: Box<esp_afe_sr_data_t>,
    // afe_iface: Box<esp_afe_sr_iface_t>,
    afe_data: SendPtr<esp_afe_sr_data_t>,
    afe_iface: SendPtr<esp_afe_sr_iface_t>,

    // 共享状态 + 条件变量
    state: Arc<Mutex<ProcessorState>>,
    cond: Arc<Condvar>,
}
impl AfeAudioProcessor {
    pub fn new(codec: Arc<Mutex<dyn AudioCodec + 'static>>) -> Result<Self, anyhow::Error> {
        // let ref_num = audio_codec.lock().unwrap().input_reference();
        let ref_num = if codec.lock().unwrap().input_reference() {
            1
        } else {
            0
        };

        // std::string input_format;
        // for (int i = 0; i < codec_->input_channels() - ref_num; i++)
        // {
        //     input_format.push_back('M');
        // }
        // for (int i = 0; i < ref_num; i++)
        // {
        //     input_format.push_back('R');
        // }
        let mut input_format = "".to_string();
        for _ in 0..(codec.lock().unwrap().input_channels() - ref_num) {
            input_format.push('M');
        }
        for _ in 0..ref_num {
            input_format.push('R');
        }

        // srmodel_list_t *models = esp_srmodel_init("model");
        // char *ns_model_name = esp_srmodel_filter(models, ESP_NSNET_PREFIX, NULL);
        // char *vad_model_name = esp_srmodel_filter(models, ESP_VADN_PREFIX, NULL);
        let model_c_str = CString::new("model").unwrap();
        let models = unsafe { esp_srmodel_init(model_c_str.as_ptr() as *const u8) };
        let ns_model_name =
            unsafe { esp_srmodel_filter(models, ESP_NSNET_PREFIX.as_ptr(), std::ptr::null()) };
        let vad_model_name =
            unsafe { esp_srmodel_filter(models, ESP_VADN_PREFIX.as_ptr(), std::ptr::null()) };

        // afe_config_t *afe_config = afe_config_init(input_format.c_str(), NULL, AFE_TYPE_VC, AFE_MODE_HIGH_PERF);
        // afe_config->aec_mode = AEC_MODE_VOIP_HIGH_PERF;
        // afe_config->vad_mode = VAD_MODE_0;
        // afe_config->vad_min_noise_ms = 100;
        // if (vad_model_name != nullptr)
        // {
        //     afe_config->vad_model_name = vad_model_name;
        // }
        let afe_config = unsafe {
            afe_config_init(
                CString::new(input_format).unwrap().as_ptr(),
                std::ptr::null_mut(),
                afe_type_t_AFE_TYPE_VC,
                afe_mode_t_AFE_MODE_HIGH_PERF,
            )
        };

        (unsafe { *afe_config }).aec_mode = aec_mode_t_AEC_MODE_VOIP_HIGH_PERF;
        (unsafe { *afe_config }).vad_mode = vad_mode_t_VAD_MODE_0;
        (unsafe { *afe_config }).vad_min_noise_ms = 100;

        if vad_model_name != std::ptr::null_mut() {
            (unsafe { *afe_config }).vad_model_name = vad_model_name;
        }

        // if (ns_model_name != nullptr)
        // {
        //     afe_config->ns_init = true;
        //     afe_config->ns_model_name = ns_model_name;
        //     afe_config->afe_ns_mode = AFE_NS_MODE_NET;
        // }
        // else
        // {
        //     afe_config->ns_init = false;
        // }
        if ns_model_name != std::ptr::null_mut() {
            (unsafe { *afe_config }).ns_init = true;
            (unsafe { *afe_config }).ns_model_name = ns_model_name;
            (unsafe { *afe_config }).afe_ns_mode = afe_ns_mode_t_AFE_NS_MODE_NET;
        } else {
            (unsafe { *afe_config }).ns_init = false;
        }

        // afe_config->afe_perferred_core = 1;
        // afe_config->afe_perferred_priority = 1;
        // afe_config->agc_init = false;
        // afe_config->memory_alloc_mode = AFE_MEMORY_ALLOC_MORE_PSRAM;
        (unsafe { *afe_config }).afe_perferred_core = 1;
        (unsafe { *afe_config }).afe_perferred_priority = 1;
        (unsafe { *afe_config }).agc_init = false;
        (unsafe { *afe_config }).memory_alloc_mode =
            afe_memory_alloc_mode_t_AFE_MEMORY_ALLOC_MORE_PSRAM;

        // #ifdef CONFIG_USE_DEVICE_AEC
        //     afe_config->aec_init = true;
        //     afe_config->vad_init = false;
        // #else
        //     afe_config->aec_init = false;
        //     afe_config->vad_init = true;
        // #endif

        // TODO:: AEC相关
        (unsafe { *afe_config }).aec_init = false;
        (unsafe { *afe_config }).vad_init = true;

        // afe_iface_ = esp_afe_handle_from_config(afe_config);
        // afe_data_ = afe_iface_->create_from_config(afe_config);
        let afe_iface: *mut esp_afe_sr_iface_t = unsafe { esp_afe_handle_from_config(afe_config) };
        let create_from_config = (unsafe { *afe_iface }).create_from_config.unwrap();
        let afe_data: *mut esp_afe_sr_data_t = unsafe { create_from_config(afe_config) };

        // xTaskCreate([](void *arg)
        // {
        // auto this_ = (AfeAudioProcessor*)arg;
        // this_->AudioProcessorTask();
        // vTaskDelete(NULL); }, "audio_communication", 4096, this, 3, NULL);

        let state = Arc::new(Mutex::new(ProcessorState {
            is_running: false,
            is_speaking: false,
            output_callback: None,
            vad_callback: None,
        }));
        let cond = Arc::new(Condvar::new());

        let mut processor = Self {
            codec: codec,
            // afe_data: afe_data,
            // afe_iface: afe_iface,
            afe_data: SendPtr(afe_data),
            afe_iface: SendPtr(afe_iface as *mut _), // iface 通常是 const，强转一下存起来
            state: state.clone(),
            cond: cond.clone(),
        };
        processor.audio_processor_task();
        Ok(processor)
    }

    fn audio_processor_task(&mut self) {
        let state_clone = self.state.clone();
        let cond_clone = self.cond.clone();

        // 因为 afe_iface 和 afe_data 是裸指针，需要手动传递给线程
        // 注意：这里假设 ESP-SR 库是线程安全的
        let afe_iface_wrapper = self.afe_iface;
        let afe_data_wrapper = self.afe_data;

        thread::Builder::new()
            .name("audio_communication".into())
            .stack_size(4096)
            .spawn(move || {
                let afe_iface = afe_iface_wrapper.as_ptr() as *const esp_afe_sr_iface_t;
                let afe_data = afe_data_wrapper.as_ptr();

                unsafe {
                    let fetch_size = ((*afe_iface).get_fetch_chunksize.unwrap())(afe_data);
                    let feed_size = ((*afe_iface).get_feed_chunksize.unwrap())(afe_data);
                    info!(
                        "Audio task started, feed: {}, fetch: {}",
                        feed_size, fetch_size
                    );

                    loop {
                        // --- 阶段 1: 等待运行信号 ---
                        let mut state_guard = state_clone.lock().unwrap();

                        while !state_guard.is_running {
                            // wait 会释放锁并挂起线程，被 notify 唤醒后会重新获取锁
                            state_guard = cond_clone.wait(state_guard).unwrap();
                        }

                        // --- 阶段 2: 释放锁，执行耗时操作 ---
                        // 必须释放锁，否则主线程调用 stop() 时会死锁
                        drop(state_guard);

                        // 调用 C 函数获取音频数据
                        // portMAX_DELAY 在 Rust 中对应 u32::MAX
                        let res = ((*afe_iface).fetch_with_delay.unwrap())(afe_data, u32::MAX);

                        // --- 阶段 3: 重新获取锁处理结果 ---
                        let mut state_guard = state_clone.lock().unwrap();

                        // 再次检查运行状态 (防止在 fetch 期间被 stop)
                        if !state_guard.is_running {
                            continue;
                        }

                        if res.is_null() || (*res).ret_value == ESP_FAIL {
                            if !res.is_null() {
                                info!("AFE Fetch Error code: {}", (*res).ret_value);
                            }
                            continue;
                        }

                        // --- 阶段 4: 处理回调 ---

                        // VAD (语音活动检测) 状态变化
                        // if let Some(ref mut vad_cb) = state_guard.vad_callback {
                        //     if (*res).vad_state == vad_state_t_VAD_SPEECH
                        //         && !state_guard.is_speaking
                        //     {
                        //         state_guard.is_speaking = true;
                        //         vad_cb(true);
                        //     } else if (*res).vad_state == vad_state_t_VAD_SILENCE
                        //         && state_guard.is_speaking
                        //     {
                        //         state_guard.is_speaking = false;
                        //         vad_cb(false);
                        //     }
                        // }
                        let mut vad_event = None; // 用于临时存储需要触发的事件

                        if (*res).vad_state == vad_state_t_VAD_SPEECH && !state_guard.is_speaking {
                            state_guard.is_speaking = true;
                            vad_event = Some(true); // 需要触发“开始说话”
                        } else if (*res).vad_state == vad_state_t_VAD_SILENCE
                            && state_guard.is_speaking
                        {
                            state_guard.is_speaking = false;
                            vad_event = Some(false); // 需要触发“停止说话”
                        }

                        // 2. 如果有事件发生，再获取回调函数进行调用
                        // 此时上面的逻辑已经结束，state_guard 的借用已经释放，可以再次借用
                        if let Some(is_speaking) = vad_event {
                            if let Some(ref mut vad_cb) = state_guard.vad_callback {
                                vad_cb(is_speaking);
                            }
                        }

                        // 输出音频数据
                        if let Some(ref mut out_cb) = state_guard.output_callback {
                            let data_len = (*res).data_size as usize / std::mem::size_of::<i16>();
                            // 从 C 指针创建切片，然后转为 Vec (发生内存拷贝)
                            let data_slice =
                                std::slice::from_raw_parts((*res).data as *const i16, data_len);
                            out_cb(data_slice.to_vec());
                        }
                    }
                }
            })
            .unwrap();
    }
}

impl AudioProcessor for AfeAudioProcessor {
    fn initialize(&mut self) {}

    fn feed(&mut self, data: &[i16]) {
        unsafe {
            ((*self.afe_iface.as_ptr()).feed.unwrap())(
                self.afe_data.as_ptr(),
                data.as_ptr() as *const _,
            );
        }
    }

    fn start(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.is_running = true;
        self.cond.notify_all();
    }

    fn stop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.is_running = false;
        // 释放锁后，线程会在下一次循环检查时暂停
        unsafe {
            ((*self.afe_iface.as_ptr()).reset_buffer.unwrap())(self.afe_data.as_ptr());
        }
    }

    fn is_running(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.is_running
    }

    fn get_feed_size(&self) -> usize {
        unsafe {
            ((*self.afe_iface.as_ptr()).get_feed_chunksize.unwrap())(self.afe_data.as_ptr())
                as usize
                * self.codec.lock().unwrap().input_channels() as usize
        }
    }

    fn enable_device_aec(&mut self, enable: bool) {
        unsafe {
            let iface = self.afe_iface.as_ptr();
            let data = self.afe_data.as_ptr();

            if enable {
                if cfg!(feature = "use_device_aec") {
                    ((*iface).disable_vad.unwrap())(data);
                    ((*iface).enable_aec.unwrap())(data);
                } else {
                    error!("Device AEC is not supported (feature not enabled)");
                }
            } else {
                ((*iface).disable_aec.unwrap())(data);
                ((*iface).enable_vad.unwrap())(data);
            }
        }
    }

    fn on_output(&mut self, callback: Box<dyn FnMut(Vec<i16>) + Send + 'static>) {
        let mut state = self.state.lock().unwrap();
        state.output_callback = Some(Box::new(callback));
    }

    fn on_vad_state_change(&mut self, callback: Box<dyn FnMut(bool) + Send + 'static>) {
        let mut state = self.state.lock().unwrap();
        state.vad_callback = Some(Box::new(callback));
    }
}

impl Drop for AfeAudioProcessor {
    fn drop(&mut self) {
        // if (afe_data_ != nullptr) {
        //     afe_iface_->destroy(afe_data_);
        // }

        unsafe {
            if !self.afe_data.as_ptr().is_null() {
                ((*self.afe_iface.as_ptr()).destroy.unwrap())(self.afe_data.as_ptr());
            }
        }
    }
}
