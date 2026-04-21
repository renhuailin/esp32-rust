use std::ffi::CString;
use std::ptr;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use esp_idf_sys::*;
use log::{error, info};

use esp_idf_sys::es32_component_esp_sr::{
    aec_mode_t_AEC_MODE_VOIP_HIGH_PERF, afe_config_init,
    afe_memory_alloc_mode_t_AFE_MEMORY_ALLOC_MORE_PSRAM, afe_mode_t_AFE_MODE_HIGH_PERF,
    afe_ns_mode_t_AFE_NS_MODE_NET, afe_type_t_AFE_TYPE_VC, esp_afe_handle_from_config,
    esp_afe_sr_data_t, esp_afe_sr_iface_t, esp_srmodel_filter, esp_srmodel_init,
    vad_mode_t_VAD_MODE_0, vad_state_t_VAD_SILENCE, vad_state_t_VAD_SPEECH, ESP_NSNET_PREFIX,
    ESP_VADN_PREFIX,
};

use crate::audio::codec::audio_codec::AudioCodec;
use crate::audio::processor::audio_processor::AudioProcessor;

// 定义回调类型
type OutputCallback = Box<dyn FnMut(Vec<i16>) + Send + 'static>;
type VadCallback = Box<dyn FnMut(bool) + Send + 'static>;

pub struct AfeAudioProcessor {
    afe_iface: *const esp_afe_sr_iface_t,
    afe_data: *mut esp_afe_sr_data_t,
    // 用于控制任务的运行/暂停
    cond: Arc<Condvar>,
    is_running: Arc<Mutex<bool>>,
    // 用于存储回调函数
    output_cb: Arc<Mutex<Option<OutputCallback>>>,
    vad_cb: Arc<Mutex<Option<VadCallback>>>,
}

impl AfeAudioProcessor {
    pub fn new(codec: Arc<Mutex<dyn AudioCodec + 'static>>) -> Result<Self, anyhow::Error> {
        info!("Initializing AfeAudioProcessor");
        // let ref_num = audio_codec.lock().unwrap().input_reference();
        let ref_num = if codec.lock().unwrap().input_reference() {
            1
        } else {
            0
        };

        let input_channels = codec.lock().unwrap().input_channels();
        info!("input_channels: {}", input_channels);

        let mut input_format = String::new();
        for _ in 0..(input_channels - ref_num) {
            input_format.push('M');
        }
        for _ in 0..ref_num {
            input_format.push('R');
        }
        let c_input_format = CString::new(input_format)?;

        unsafe {
            // 初始化 SR 模型
            let model_c_str = CString::new("model")?;
            let models = esp_srmodel_init(model_c_str.as_ptr() as *const _);

            if models.is_null() {
                error!("没有找到 model 分区！");
            } else {
                let count = (*models).num as usize;
                info!("成功加载模型列表，共有 {} 个模型，开始遍历：", count);

                for i in 0..count {
                    // 通过指针偏移访问第 i 个元素的名称
                    // model_name 是 *mut *mut c_char，即 char**
                    let name_ptr = *((*models).model_name.add(i));

                    if !name_ptr.is_null() {
                        let name = CStr::from_ptr(name_ptr).to_string_lossy();
                        info!("找到模型文件索引 {}: {}", i, name);
                    }
                }
            }

            let ns_model_name = esp_srmodel_filter(
                models,
                ESP_NSNET_PREFIX.as_ptr() as *const _,
                ptr::null_mut(),
            );
            let vad_model_name = esp_srmodel_filter(
                models,
                ESP_VADN_PREFIX.as_ptr() as *const _,
                ptr::null_mut(),
            );

            // 配置 AFE
            let afe_config = afe_config_init(
                c_input_format.as_ptr(),
                ptr::null_mut(),
                afe_type_t_AFE_TYPE_VC,
                afe_mode_t_AFE_MODE_HIGH_PERF,
            );

            (*afe_config).aec_mode = aec_mode_t_AEC_MODE_VOIP_HIGH_PERF;
            (*afe_config).vad_mode = vad_mode_t_VAD_MODE_0;
            (*afe_config).vad_min_noise_ms = 100;
            if !vad_model_name.is_null() {
                (*afe_config).vad_model_name = vad_model_name;
            }
            if !ns_model_name.is_null() {
                (*afe_config).ns_init = true;
                (*afe_config).ns_model_name = ns_model_name;
                (*afe_config).afe_ns_mode = afe_ns_mode_t_AFE_NS_MODE_NET;
            } else {
                (*afe_config).ns_init = false;
            }

            (*afe_config).afe_perferred_core = 1;
            (*afe_config).afe_perferred_priority = 5;
            (*afe_config).memory_alloc_mode = afe_memory_alloc_mode_t_AFE_MEMORY_ALLOC_MORE_PSRAM;

            let afe_iface = esp_afe_handle_from_config(afe_config);
            let afe_data = ((*afe_iface).create_from_config.unwrap())(afe_config);

            let proc = Self {
                afe_iface,
                afe_data,
                cond: Arc::new(Condvar::new()),
                is_running: Arc::new(Mutex::new(false)),
                output_cb: Arc::new(Mutex::new(None)),
                vad_cb: Arc::new(Mutex::new(None)),
            };

            proc.spawn_task();
            Ok(proc)
        }
    }

    fn spawn_task(&self) {
        let is_running = self.is_running.clone();
        let cond = self.cond.clone();
        let afe_iface = self.afe_iface;
        let afe_data = self.afe_data;
        let output_cb = self.output_cb.clone();
        let vad_cb = self.vad_cb.clone();

        // thread::Builder::new()
        //     .name("afe_task".into())
        //     .stack_size(16 * 1024)
        //     .spawn(move || unsafe {
        //         loop {
        //             let mut run = is_running.lock().unwrap();
        //             while !*run {
        //                 run = cond.wait(run).unwrap();
        //             }
        //             drop(run);

        //             let res = ((*afe_iface).fetch_with_delay.unwrap())(afe_data, u32::MAX);
        //             if res.is_null() || (*res).ret_value == ESP_FAIL {
        //                 continue;
        //             }

        //             // VAD 回调
        //             if let Ok(mut cb) = vad_cb.lock() {
        //                 if let Some(ref mut func) = *cb {
        //                     func((*res).vad_state == vad_state_t_VAD_SPEECH);
        //                 }
        //             }

        //             // 输出数据回调
        //             if let Ok(mut cb) = output_cb.lock() {
        //                 if let Some(ref mut func) = *cb {
        //                     let data_len = (*res).data_size as usize / 2;
        //                     let slice =
        //                         std::slice::from_raw_parts((*res).data as *const i16, data_len);
        //                     func(slice.to_vec());
        //                 }
        //             }
        //         }
        //     })
        //     .unwrap();

        // 定义线程内的循环逻辑
        let task_closure: Box<dyn FnOnce() + Send> = Box::new(move || {
            let mut is_speaking = false;
            unsafe {
                loop {
                    // 等待运行信号
                    let mut run = is_running.lock().unwrap();
                    while !*run {
                        run = cond.wait(run).unwrap();
                    }
                    drop(run);

                    // 执行 fetch
                    let res = ((*afe_iface).fetch_with_delay.unwrap())(afe_data, u32::MAX);

                    if res.is_null() || (*res).ret_value == ESP_FAIL {
                        continue;
                    }

                    // VAD 处理
                    if let Ok(mut cb) = vad_cb.lock() {
                        if let Some(ref mut func) = *cb {
                            let speaking = (*res).vad_state == vad_state_t_VAD_SPEECH;
                            if speaking != is_speaking {
                                is_speaking = speaking;
                                func(speaking);
                            }
                        }
                    }

                    // Output 处理
                    if let Ok(mut cb) = output_cb.lock() {
                        if let Some(ref mut func) = *cb {
                            let data_len = (*res).data_size as usize / 2;
                            let slice =
                                std::slice::from_raw_parts((*res).data as *const i16, data_len);
                            func(slice.to_vec());
                        }
                    }
                }
            }
        });
        let closure_box = Box::new(task_closure);
        let closure_ptr = Box::into_raw(closure_box);

        info!("try to call xTaskCreatePinnedToCore in the unsafe block");
        unsafe {
            let res = esp_idf_sys::xTaskCreatePinnedToCore(
                Some(c_task_trampoline),
                b"audio_processor\0".as_ptr() as *const u8,
                16 * 1024,
                closure_ptr as *mut c_void,
                3,
                ptr::null_mut(),
                1,
            );
            // if res != esp_idf_sys::pdPass {
            //     // 如果创建失败，记得收回内存，否则会泄漏
            //     let _ = Box::from_raw(closure_ptr);
            //     error!("Failed to create task");
            // }
        }
    }

    // --- 接口实现 ---
}

impl AudioProcessor for AfeAudioProcessor {
    fn initialize(&mut self) {
        info!("initialize afe audio processor");
    }

    fn feed(&mut self, data: &[i16]) {
        unsafe {
            ((*self.afe_iface).feed.unwrap())(self.afe_data, data.as_ptr());
        }
    }

    fn start(&mut self) {
        let mut run = self.is_running.lock().unwrap();
        *run = true;
        self.cond.notify_all();
    }

    fn stop(&mut self) {
        let mut run = self.is_running.lock().unwrap();
        *run = false;
        unsafe {
            ((*self.afe_iface).reset_buffer.unwrap())(self.afe_data);
        }
    }

    fn is_running(&self) -> bool {
        *self.is_running.lock().unwrap()
    }

    fn get_feed_size(&self) -> usize {
        unsafe {
            ((*self.afe_iface).get_feed_chunksize.unwrap())(self.afe_data) as usize
                * self.codec.lock().unwrap().input_channels() as usize
        }
    }

    fn on_output<F>(&self, callback: F)
    where
        F: FnMut(Vec<i16>) + Send + 'static,
    {
        *self.output_cb.lock().unwrap() = Some(Box::new(callback));
    }

    fn on_vad_state_change<F>(&self, callback: F)
    where
        F: FnMut(bool) + Send + 'static,
    {
        *self.vad_cb.lock().unwrap() = Some(Box::new(callback));
    }

    fn enable_device_aec(&self, enable: bool) {
        unsafe {
            if enable {
                ((*self.afe_iface).disable_vad.unwrap())(self.afe_data);
                ((*self.afe_iface).enable_aec.unwrap())(self.afe_data);
            } else {
                ((*self.afe_iface).disable_aec.unwrap())(self.afe_data);
                ((*self.afe_iface).enable_vad.unwrap())(self.afe_data);
            }
        }
    }
}

impl Drop for AfeAudioProcessor {
    fn drop(&mut self) {
        unsafe {
            ((*self.afe_iface).destroy.unwrap())(self.afe_data);
        }
    }
}
