use crate::{
    audio::{
        codec::{types::AudioStreamPacket, AUDIO_INPUT_SAMPLE_RATE},
        processor::wake_word_service::WakeWordService,
    },
    common::converter::i16_slice_to_bytes,
    display::{lcd::st7789::LcdSt7789, Display},
    wifi::ssid_manager::{self, SsidMananger},
};
use anyhow::{Error, Result};
use chrono::Utc;
use esp_idf_hal::{
    delay::BLOCK,
    i2s::{I2sBiDir, I2sDriver},
    task::thread::ThreadSpawnConfiguration,
};
use std::{
    collections::VecDeque,
    ffi::{c_void, CStr, CString},
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, channel, Receiver, Sender, SyncSender},
        Arc, Mutex, MutexGuard,
    },
    thread,
    time::Duration,
};

use esp_idf_sys::{
    es32_component_esp_sr::wake_word_info_t, esp_partition_find, esp_partition_get,
    esp_partition_next, esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_OTA_0,
    esp_partition_type_t_ESP_PARTITION_TYPE_APP, i2s_port_t_I2S_NUM_0, i2s_start, i2s_stop,
    i2s_zero_dma_buffer, setenv, settimeofday, timeval, tzset,
};
use log::{error, info, warn};

use crate::{
    audio::{
        codec::{
            audio_codec::AudioCodec,
            opus::{decoder::OpusAudioDecoder, encoder::OpusAudioEncoder},
            MAX_AUDIO_PACKETS_IN_QUEUE, OPUS_FRAME_DURATION_MS,
        },
        processor::{audio_processor::AudioProcessor, no_audio_processor::NoAudioProcessor},
    },
    boards::{board::Board, jianglian_s3cam_board},
    common::{
        application_context::ApplicationContext,
        converter::bytes_to_i16_slice,
        enums::{AbortReason, AecMode, DeviceState, ListeningMode},
        event::AppEvent,
    },
    protocols::{protocol::Protocol, websocket::ws_protocol::WebSocketProtocol},
    utils::ffi::c_task_trampoline,
    wifi::wifi_driver::{Esp32WifiDriver, WifiStation},
};

use embedded_svc::http::client::Client as HttpClient;
use esp_idf_svc::{
    http::{
        client::{EspHttpConnection, Response},
        Method,
    },
    io,
    ota::{EspFirmwareInfoLoad, EspOta, EspOtaUpdate, FirmwareInfo},
};

// 使用VecDeque作为缓冲区，因为它在头部移除元素时效率很高
pub type AudioBuffer = VecDeque<u8>;

// 共享状态结构体,主要用于音频测试模式保存PCM数据。
pub struct SharedAudioState {
    pub buffer: Mutex<AudioBuffer>,
    pub audio_packet_buffer: Mutex<VecDeque<AudioStreamPacket>>, // 我们可以添加一个Condvar，以便在录音满或播放空时进行等待

    pub pcm_buffer: Mutex<VecDeque<i16>>, //用于回放的pcm数据buffer

    audio_decode_queue: Mutex<VecDeque<AudioStreamPacket>>, //待解码的音频队列
    busy_decoding_audio: AtomicBool, //正在解码音频,TODO:: 在c++代码中，如果正在解码音频，则不播放音频
    abort_speaking: AtomicBool,      //是否中断speaking
}

impl SharedAudioState {
    pub fn new() -> Self {
        let audio_decode_queue = Mutex::new(VecDeque::<AudioStreamPacket>::with_capacity(
            MAX_AUDIO_PACKETS_IN_QUEUE,
        ));
        Self {
            buffer: Mutex::new(VecDeque::new()),
            audio_packet_buffer: Mutex::new(VecDeque::new()),
            pcm_buffer: Mutex::new(VecDeque::new()),
            audio_decode_queue,
            busy_decoding_audio: false.into(),
            abort_speaking: false.into(),
        }
    }
}

const VERSION: &str = env!("CARGO_PKG_VERSION");
fn check_new_version(mac_address: &str) -> anyhow::Result<()> {
    info!("check_new_version, current version is {}", VERSION);

    let mut client = HttpClient::wrap(EspHttpConnection::new(&Default::default())?);
    check_for_updates(&mut client, mac_address)?;
    Ok(())
}

mod http_status {
    pub const OK: u16 = 200;
    pub const NOT_MODIFIED: u16 = 304;
}

pub fn check_for_updates(
    client: &mut HttpClient<EspHttpConnection>,
    mac_address: &str,
) -> anyhow::Result<()> {
    let mut ota = EspOta::new()?;

    let current_version = VERSION;
    info!("Current version: {current_version}");

    info!("Checking for updates...");
    let request_body = br#"{"app_version":"1.0.0"}"#;
    let content_len = request_body.len().to_string();
    let headers = [
        // ("Accept", "application/octet-stream"),
        // ("X-Esp32-Version", &current_version),
        ("content-type", "application/json"),
        ("Content-Length", &content_len),
        ("device-id", mac_address),
    ];

    // let ota_firmware_url = "http://192.168.1.145:3000/api/v1/ota/update";
    let ota_check_url = "http://192.168.1.174:3003/api/ota/check";

    let mut request = client.request(Method::Post, ota_check_url, &headers)?;
    // 2. 写入 body
    request.write(request_body)?;

    let mut response = request.submit()?;

    let mut body = [0_u8; 3048];

    if response.status() == http_status::NOT_MODIFIED {
        info!("OTA: Already up to date");
    } else if response.status() == http_status::OK {
        // TODO:: 这里需要解析response的body，获取到ota_firmware_url
        let read = response.read(&mut body)?;

        let body_str = String::from_utf8_lossy(&body[..read]).into_owned();
        info!("OTA: body: {body_str}");
        info!("Body (truncated to 3K):\n{:?}", &body_str);
        let json = serde_json::from_str::<serde_json::Value>(&body_str)?;
        let ts = json["server_time"]["timestamp"].as_i64().unwrap_or(0);
        let offset = json["server_time"]["timezone_offset"].as_i64().unwrap_or(0) as i32;
        update_local_time(ts, offset)?;
        return Ok(());

        info!("OTA: An update is available, updating...");
        // let mut update = ota.initiate_update()?;

        info!("print app partition...");
        unsafe {
            let mut it = esp_partition_find(
                esp_partition_type_t_ESP_PARTITION_TYPE_APP,
                esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_OTA_0,
                ptr::null(),
            );
            while !it.is_null() {
                let part = esp_partition_get(it);
                let name = CStr::from_ptr((*part).label.as_ptr()).to_string_lossy();
                info!(
                    "Found app partition: {}, offset: 0x{:x}, size: 0x{:x}",
                    name,
                    (*part).address,
                    (*part).size
                );
                it = esp_partition_next(it);
            }
        }

        info!("initiate update...");
        match ota.initiate_update() {
            Ok(mut update) => {
                info!("initiate updated");
                match download_update(response, &mut update) {
                    Ok(_) => {
                        info!("Update done. Restarting...");
                        update.complete()?;
                        esp_idf_svc::hal::reset::restart();
                    }
                    Err(err) => {
                        error!("Update failed: {err}");
                        update.abort()?;
                    }
                };
            }
            Err(err) => {
                error!("initiate update failed: {err}");
            }
        }
    }

    Ok(())
}

fn download_update(
    mut response: Response<&mut EspHttpConnection>,
    update: &mut EspOtaUpdate<'_>,
) -> anyhow::Result<()> {
    let mut buffer = [0_u8; 1024];

    // You can optionally read the firmware metadata header.
    // It contains information like version and signature you can check before continuing the update
    let update_info = read_firmware_info(&mut buffer, &mut response, update)?;
    info!("Update version: {}", update_info.version);

    io::utils::copy(response, update, &mut buffer)?;

    Ok(())
}

fn read_firmware_info(
    buffer: &mut [u8],
    response: &mut Response<&mut EspHttpConnection>,
    update: &mut EspOtaUpdate,
) -> anyhow::Result<FirmwareInfo> {
    let update_info_load = EspFirmwareInfoLoad {};
    let mut update_info = FirmwareInfo {
        version: Default::default(),
        released: Default::default(),
        description: Default::default(),
        signature: Default::default(),
        download_id: Default::default(),
    };

    loop {
        let n = response.read(buffer)?;
        update.write(&buffer[0..n])?;
        if update_info_load.fetch(&buffer[0..n], &mut update_info)? {
            return Ok(update_info);
        }
    }
}

fn update_local_time(timestamp_ms: i64, timezone_offset_min: i32) -> anyhow::Result<()> {
    // 1. 毫秒 -> 秒 + 微秒
    let sec = timestamp_ms / 1000;
    let usec = (timestamp_ms % 1000) * 1000;

    let tv = timeval {
        tv_sec: sec as _,
        tv_usec: usec as _,
    };

    // 2. 设置系统时间 (第二个参数已废弃, 传 null)
    let ret = unsafe { settimeofday(&tv, std::ptr::null()) };
    if ret != 0 {
        return Err(anyhow::anyhow!("settimeofday failed"));
    }

    // 3. 设置时区
    // POSIX TZ 格式: 东八区 = CST-8 (符号与日常习惯相反, 负号表示东)
    let hours = timezone_offset_min / 60;
    let mins = timezone_offset_min.abs() % 60;
    let tz_str = if mins == 0 {
        format!("CST-{}", hours)
    } else {
        format!("CST-{}:{:02}", hours, mins)
    };

    let c_tz = CString::new(tz_str)
        .map_err(|_| "invalid tz string")
        .map_err(|e| anyhow::anyhow!("invalid tz string: {}", e))?;

    unsafe {
        setenv("TZ".as_ptr(), c_tz.as_ptr(), 1); // 1 = overwrite
        tzset();
    }

    log::info!(
        "System time updated: {} ms, TZ offset {} min",
        timestamp_ms,
        timezone_offset_min
    );
    Ok(())
}

pub struct Application {
    state: DeviceState,
    protocol: WebSocketProtocol,
    board: Box<dyn Board<WifiDriver = Esp32WifiDriver, DisplayDriver = LcdSt7789>>,

    //用于处理内部事件的channel
    inner_sender: Sender<AppEvent>,
    inner_receiver: Receiver<AppEvent>,

    //用于播放pcm的channel
    inner_pcm_tx: SyncSender<Vec<u8>>,
    inner_pcm_rx: Option<Receiver<Vec<u8>>>,

    aec_mode: AecMode,
    listening_mode: ListeningMode,

    opus_encoder: Arc<Mutex<OpusAudioEncoder>>,
    opus_decoder: Arc<Mutex<OpusAudioDecoder>>,

    audio_processor: Arc<Mutex<dyn AudioProcessor>>,
    audio_packet_queue: Arc<Mutex<VecDeque<AudioStreamPacket>>>, //待发送的音频队列

    wake_word_service: Arc<Mutex<Option<WakeWordService>>>,

    decode_task_sender: Sender<AppEvent>,
    decode_task_receiver: Option<Receiver<AppEvent>>,
    audio_test_mode: bool, //音频测试模式,在这个模式下，并不真的发送音频数据到服务器端，
    // 而是直接保存在这个字段的buffer里，然后在音箱端解码，播放出来，
    // 主要是用于测试音频采集是否正常及解码是否正常。
    shared_audio_state: Arc<SharedAudioState>,

    audio_format: String, // PCM, OPUS，注意要与服务器端的格式一致
}
impl Application {
    pub fn new() -> Result<Self> {
        let (inner_sender, inner_receiver): (Sender<AppEvent>, Receiver<AppEvent>) = channel();

        let (decode_task_sender, decode_task_receiver): (Sender<AppEvent>, Receiver<AppEvent>) =
            channel();

        let app_context = ApplicationContext {
            app_event_sender: inner_sender.clone(),
        };

        let mut board = Box::new(jianglian_s3cam_board::JiangLianS3CamBoard::new(
            app_context,
        )?);

        let sender = inner_sender.clone();
        board.on_speak_button_clicked(Box::new(move || {
            // info!("Touch button clicked");
            if let Err(e) = sender.send(AppEvent::SpeakButtonClicked) {
                log::error!("Failed to send SpeakButtonClicked event: {:?}", e);
            }
        }));

        let sender1 = inner_sender.clone();
        board.on_volume_button_clicked(Box::new(move || {
            // info!("Volume button clicked");
            if let Err(e) = sender1.send(AppEvent::VolumeButtonClicked) {
                log::error!("Failed to send VolumeButtonClicked event: {:?}", e);
            }
        }));

        let sender2 = inner_sender.clone();
        board.on_volume_button_long_pressed(Box::new(move || {
            info!("Volume button long pressed!");
            if let Err(e) = sender2.send(AppEvent::VolumeButtonLongPressed) {
                log::error!("Failed to send VolumeButtonLongPressed event: {:?}", e);
            }
        }));

        board.set_on_wifi_connected_callback(Box::new(move |ssid: String, mac_address: String| {
            info!("Volume button long pressed!");
            // info!("check new version ...");
            if let Err(e) = check_new_version(&mac_address) {
                log::error!("Failed to check new version {:?}", e);
            }

            //print current systime.
            let now = Utc::now().to_rfc3339();
            info!("current systime: {}", now);

            // update ssid last connect time.
            let mut ssid_manager = SsidMananger::get_instance();
            if let Err(e) = ssid_manager.update_ssid_last_connect_time(&ssid, &now) {
                log::error!("Failed to update ssid last connect time {:?}", e);
            }
        }));

        board.init()?;
        info!("board init success");

        let mac_address = board.get_wifi_driver().get_mac_address()?;
        info!("MAC address: {}", mac_address);
        let sender_for_protocol = inner_sender.clone();
        let protocol = WebSocketProtocol::new(mac_address.as_str(), sender_for_protocol);

        //待发送的音频队列
        let audio_packet_queue = Arc::new(Mutex::new(
            VecDeque::<AudioStreamPacket>::with_capacity(MAX_AUDIO_PACKETS_IN_QUEUE),
        ));

        let (input_channels, input_reference) = {
            let codec = board.get_audio_codec().clone();
            let input_channels = codec.lock().unwrap().input_channels();
            let input_reference = codec.lock().unwrap().input_reference();
            (input_channels, input_reference)
        };

        // let audio_processor = Arc::new(Mutex::new(
        //     AfeAudioProcessor::new(input_channels as usize, input_reference).unwrap(),
        // ));

        //先使用NoAudioProcessor，等以后有时间再改成AfeAudioProcessor，因为我测试很久，AfeAudioProcessor的总是报堆栈溢出。
        let audio_processor = Arc::new(Mutex::new(NoAudioProcessor::new(
            AUDIO_INPUT_SAMPLE_RATE,
            OPUS_FRAME_DURATION_MS as u32,
        )));

        let shared_audio_state = Arc::new(SharedAudioState::new());

        let sample_rate = AUDIO_INPUT_SAMPLE_RATE as i32; //# 采样率固定为16000Hz
        let channels = 2; //# 单声道
        info!("create opus encoder");
        let opus_encoder = Arc::new(Mutex::new(
            OpusAudioEncoder::new(
                sample_rate,
                channels,
                OPUS_FRAME_DURATION_MS.try_into().unwrap(),
            )
            .unwrap(),
        ));
        opus_encoder.lock().unwrap().set_complexity(5);

        let opus_decoder = Arc::new(Mutex::new(
            OpusAudioDecoder::new(
                sample_rate,
                channels,
                OPUS_FRAME_DURATION_MS.try_into().unwrap(),
            )
            .unwrap(),
        ));

        // 使用 sync_channel 创建一个带缓冲的 channel，防止内存无限制增长
        let (pcm_tx, pcm_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(10);
        // ── 唤醒诊断开关 ─────────────────────────────────────────────────────
        // 背景：ES7210 std（非TDM）模式下 I2S 两个槽位 = MIC1 + MIC2。
        // 若 ch1 实为第二个麦克风而非播放回采参考，AFE 以 "MR" 格式把 MIC2
        // 当 AEC 参考去消 MIC1 的"回声"——两路相邻 mic 的人声高度相关，
        // AEC 会逐渐收敛到把人声也消掉 → 唤醒几乎必败，且越用越难唤醒。
        // 此开关临时关闭 AEC（AFE 单声道 "M"）：codec 照常读双声道但只喂
        // MIC1，配合 audio_loop 里 wake feed alive 分声道探针，
        // 一次烧录即可确诊 + 验证。若唤醒恢复可靠 => 假设成立。
        const WAKE_DISABLE_AEC_DIAG: bool = true;

        let (wake_channels, wake_reference) = if WAKE_DISABLE_AEC_DIAG {
            info!("[wake-diag] AEC disabled: AFE uses single mic channel (M)");
            (1usize, false)
        } else {
            (input_channels as usize, input_reference)
        };
        let wake_word_service: Arc<Mutex<Option<WakeWordService>>> =
            match WakeWordService::new(wake_channels, wake_reference) {
                Ok(service) => Arc::new(Mutex::new(Some(service))),
                Err(_) => Arc::new(Mutex::new(None)),
            };

        let instance = Self {
            state: DeviceState::Idle,
            protocol,
            // device_id: mac_address,
            board,
            inner_sender,
            inner_receiver,
            decode_task_sender,
            decode_task_receiver: Some(decode_task_receiver),
            aec_mode: AecMode::Off,
            listening_mode: ListeningMode::AutoStop,
            audio_processor: audio_processor,
            audio_packet_queue,
            audio_test_mode: false,
            shared_audio_state,
            opus_decoder,
            opus_encoder,
            inner_pcm_tx: pcm_tx,
            inner_pcm_rx: Some(pcm_rx),
            audio_format: "opus".to_string(),
            wake_word_service: wake_word_service,
        };
        Ok(instance)
    }

    pub fn start(&mut self) -> Result<(), Error> {
        self.set_device_state(DeviceState::Starting);
        let codec_arc = self.board.get_audio_codec();
        // let codec_arc = Arc::new(Mutex::new(codec));

        // let codec = self.board.get_audio_codec();
        codec_arc.lock().unwrap().start();

        info!("starting  network");
        /* Wait for the network to be ready */
        match self.board.start_network() {
            Ok(_) => {
                info!("network started!");
            }
            Err(err) => {
                error!("network start failed: {:?}", err);
                return Err(err);
            }
        }

        // self.protocol.set_on_close_handler(|| {
        //     // self.board.set_save_power_mode(true);
        //     self.set_device_state(DeviceState::Idle);
        //     Ok(())
        // });
        // let sender = self.inner_sender.clone();
        // self.protocol.on_incoming_text(move |text| {
        //     info!("Received text message: {}", text);
        //     if let Err(e) = sender.send(XzEvent::WebsocketTextMessageReceived(text.to_string())) {
        //         log::error!("Failed to send WebsocketTextMessageReceived event: {:?}", e);
        //     }
        //     Ok(())
        // })?;
        // let sender1 = self.inner_sender.clone();
        // self.protocol.on_incoming_audio(move |packet| {
        //     self.set_device_state(DeviceState::Activating);
        //     Ok(())
        // })?;

        let inner_sender = self.inner_sender.clone();
        self.protocol.on_network_error(move |err| {
            if let Err(e) = inner_sender.send(AppEvent::ProtocolNetworkError(err.to_string())) {
                log::error!("Failed to send ProtocolNetworkError event: {:?}", e);
            }
            Ok(())
        });

        let codec_clone = Arc::clone(&codec_arc);
        let codec_clone_for_pcm_player = Arc::clone(&codec_arc);
        let (pcm_tx, pcm_rx) = mpsc::channel::<Vec<i16>>();

        let inner_sender = self.inner_sender.clone();

        let audio_test_mode = self.audio_test_mode.clone();
        let audio_state = Arc::clone(&self.shared_audio_state);

        let opus_encoder_arc = Arc::clone(&self.opus_encoder);

        let audio_format = self.audio_format.clone();
        let encode_thread = thread::Builder::new()
            .name("encoder_task".into())
            .stack_size(32 * 1024)
            .spawn(move || {
                let opus_encoder = Arc::clone(&opus_encoder_arc);
                for pcm_data in pcm_rx {
                    // 在这里做编码，环境单纯，没有锁竞争
                    // 打印数据长度，排查问题
                    // info!("Encoding frame size: {}", pcm_data.len());

                    let inner_sender1 = inner_sender.clone();
                    let encoder = Arc::clone(&opus_encoder);
                    let audio_state1 = Arc::clone(&audio_state);

                    if audio_test_mode {
                        // sender.send(XzEvent::AudioPacketReceived(packet)).unwrap();
                        // audio_state1
                        //     .pcm_buffer
                        //     .lock()
                        //     .unwrap()
                        //     .extend(pcm_data.as_slice());
                        let pcm_u8 = i16_slice_to_bytes(pcm_data.as_slice()).unwrap();
                        codec_clone_for_pcm_player
                            .lock()
                            .unwrap()
                            .test_play_pcm(pcm_u8)
                            .unwrap();
                        continue;
                    }

                    let sender = inner_sender1.clone();

                    if audio_format == "pcm" {
                        let pcm_u8 = i16_slice_to_bytes(&pcm_data.as_slice()).unwrap();

                        let packet = AudioStreamPacket {
                            sample_rate: AUDIO_INPUT_SAMPLE_RATE as i32,
                            frame_duration: OPUS_FRAME_DURATION_MS as i32,
                            timestamp: 0,
                            payload: pcm_u8.to_vec(),
                        };
                        if let Err(e) = sender.send(AppEvent::AddAudioPacketToQueue(packet)) {
                            error!("Failed to send audio packet: {:?}", e);
                            return;
                        }
                    } else {
                        let result = encoder
                            .lock()
                            .map_err(|e| {
                                error!("Encoder lock poisoned: {:?}", e);
                                // 可以选择 clear_poison() 或者直接返回
                            })
                            .unwrap()
                            .encode(pcm_data, &mut move |opus_data: Vec<u8>| {
                                // info!("编码完成，add audio packet to queue");
                                let packet = AudioStreamPacket {
                                    sample_rate: AUDIO_INPUT_SAMPLE_RATE as i32,
                                    frame_duration: OPUS_FRAME_DURATION_MS as i32,
                                    timestamp: 0,
                                    payload: opus_data,
                                };

                                if audio_test_mode {
                                    // sender.send(XzEvent::AudioPacketReceived(packet)).unwrap();
                                    audio_state1
                                        .audio_packet_buffer
                                        .lock()
                                        .unwrap()
                                        .push_back(packet);
                                } else {
                                    if let Err(e) =
                                        sender.send(AppEvent::AddAudioPacketToQueue(packet))
                                    {
                                        error!("Failed to send audio packet: {:?}", e);
                                        return;
                                    }
                                }
                            });

                        match result {
                            Ok(_) => {}
                            Err(e) => {
                                error!("Encode error: {:?}", e);
                            }
                        }
                    }
                }
            });
        match encode_thread {
            Ok(_) => {}
            Err(_) => {
                error!("Failed to create encode thread");
            }
        }

        let audio_processor = Arc::clone(&self.audio_processor);

        let audio_state = Arc::clone(&self.shared_audio_state);

        audio_processor
            .lock()
            .unwrap()
            .on_output(Box::new(move |data| {
                // info!("on audio processor output,data length: {}", data.len());
                // info!("on audio processor output data: {:?}", data);

                // 发送到编码线程,编码成opus.
                if let Err(e) = pcm_tx.send(data) {
                    // 如果发送失败（比如编码线程挂了），打印个日志，不要 panic
                    error!("Failed to send PCM to encoder: {:?}", e);
                }
            }));
        let pcm_tx = self.inner_pcm_tx.clone();
        let audio_state = Arc::clone(&self.shared_audio_state);
        let wake_word_service_for_loop = self.wake_word_service.clone();

        let task_closure: Box<dyn FnOnce() + Send> = Box::new(move || {
            audio_loop(
                codec_clone,
                audio_processor,
                audio_state,
                pcm_tx,
                wake_word_service_for_loop,
            );
        });

        let closure_box = Box::new(task_closure);
        let closure_ptr = Box::into_raw(closure_box);

        info!("try to call xTaskCreatePinnedToCore in the unsafe block");
        unsafe {
            let res = esp_idf_sys::xTaskCreatePinnedToCore(
                Some(c_task_trampoline),
                b"audio_loop\0".as_ptr() as *const u8,
                // 4096 * 3,
                16 * 1024,
                closure_ptr as *mut c_void,
                8,
                ptr::null_mut(),
                1,
            );
            info!("create audio loop task - res: {}", res);
            if res != 1 {
                // 如果创建失败，记得收回内存，否则会泄漏
                let _ = Box::from_raw(closure_ptr);
                error!("Failed to create task");
            }
        }

        info!("启动解码线程 start_output_audio ...");
        // self.start_output_audio();

        self.set_device_state(DeviceState::Idle);

        info!("启动唤醒词服务...");
        let wake_word_service_clone = self.wake_word_service.clone();

        // 注册唤醒回调：检测到唤醒词时发送内部事件，由主事件循环统一处理
        let inner_sender_for_wake = self.inner_sender.clone();
        {
            let mut guard = wake_word_service_clone.lock().unwrap();
            if let Some(service) = guard.as_mut() {
                service.on_wake_word_detected(Box::new(move |wake_word| {
                    info!("Wake word detected: {}", wake_word);
                    if let Err(e) =
                        inner_sender_for_wake.send(AppEvent::WakeWordDetected(wake_word))
                    {
                        error!("Failed to send WakeWordDetected event: {:?}", e);
                    }
                }));
                // start_detection 内部现在会先复位 AFE 缓冲再启动检测，
                // 开机路径与按键路径行为一致，无需在此显式复位。
                service.start_detection();
            }
        }

        let codec_for_opus_player = Arc::clone(&codec_arc);

        let pcm_player_codec = Arc::clone(&codec_for_opus_player);

        //启动音频输出子线程
        info!("启动音频输出线程 start pcm_player_thread ...");
        if let Some(pcm_rx) = self.inner_pcm_rx.take() {
            ThreadSpawnConfiguration {
                name: Some(c"pcm_player_thread"),
                stack_size: 8 * 1024,
                priority: 10,
                pin_to_core: Some(1.into()), // 绑定到 Core 1

                // 关键点：虽然这里没有直接的 "stack_in_psram" 字段，
                // 但我们可以通过设置 inherit 为 false 来避免继承父线程的配置
                inherit: false,
                ..Default::default()
            }
            .set()
            .unwrap();

            let _ = thread::spawn(move || {
                // let mut opus_decoder = ...;
                for pcm_packet in pcm_rx {
                    pcm_player_codec
                        .lock()
                        .unwrap()
                        .output_data(&pcm_packet)
                        .unwrap();
                }
            });
            ThreadSpawnConfiguration::default().set().unwrap();
        }

        info!("Enter event loop,开始处理内部事件 ...");

        self.audio_alert("success");

        // 初始化显示假电池电量（75%），后续接入真实电池数据后替换
        self.board.get_display().show_battery_level(75);

        // 启动 WiFi 信号强度定时刷新
        let wifi_signal_sender = self.inner_sender.clone();
        let _ = thread::Builder::new()
            .name("wifi_signal".into())
            .stack_size(4 * 1024)
            .spawn(move || loop {
                thread::sleep(Duration::from_secs(5));
                if let Err(e) = wifi_signal_sender.send(AppEvent::RefreshWifiSignal) {
                    log::error!("Failed to send RefreshWifiSignal: {:?}", e);
                    break;
                }
            });

        // 处理内部事件
        self.event_loop()?;
        Ok(())
    }

    ///播放音频提醒
    pub fn audio_alert(&mut self, message: &str) {
        self.reset_decoder();
        self.play_p3_audio(message);
    }

    fn event_loop(&mut self) -> Result<(), Error> {
        let mut shared_pcm_buffer: Vec<i16> = Vec::with_capacity(4096);
        let inner_sender = self.inner_sender.clone();
        let audio_state = Arc::clone(&self.shared_audio_state);
        let audio_test_mode = self.audio_test_mode;
        let audio_packet_send_queue_arc = Arc::clone(&self.audio_packet_queue);
        let codec_for_opus_player = Arc::clone(&self.board.get_audio_codec());

        let codec_clone_for_pcm_player = Arc::clone(&self.board.get_audio_codec());

        loop {
            let opus_decoder = Arc::clone(&self.opus_decoder);
            match self.inner_receiver.recv() {
                Ok(event) => {
                    match event {
                        AppEvent::WakeWordDetected(wake_word) => {
                            info!("唤醒词触发: {}", wake_word);
                            // 对齐 C++ Application::OnWakeWordDetected（CONFIG_USE_AFE_WAKE_WORD 路径）
                            match self.state {
                                DeviceState::Idle => {
                                    // 1. 编码缓存的唤醒词音频（约 2 秒 PCM -> opus）
                                    {
                                        let mut guard = self.wake_word_service.lock().unwrap();
                                        if let Some(service) = guard.as_mut() {
                                            service.encode_wake_word_data();
                                        }
                                    }
                                    // 2. 音频通道未打开时先连接服务器
                                    if !self.protocol.is_audio_channel_opened() {
                                        self.set_device_state(DeviceState::Connecting);
                                        // 先清理旧连接（超时/僵尸状态），
                                        // 避免在已存在 client 的情况下叠加 websocket 任务
                                        let _ = self.protocol.close_audio_channel();
                                        match self.protocol.open_audio_channel() {
                                            Ok(true) => {}
                                            _ => {
                                                error!("Failed to open audio channel");
                                                // 连接失败，恢复唤醒检测等待下次唤醒
                                                if let Some(service) = self
                                                    .wake_word_service
                                                    .lock()
                                                    .unwrap()
                                                    .as_mut()
                                                {
                                                    service.start_detection();
                                                }
                                                continue;
                                            }
                                        }
                                    }
                                    // 3. 上行唤醒词音频（服务器可用作上下文）
                                    loop {
                                        let opus_data = {
                                            let mut guard = self.wake_word_service.lock().unwrap();
                                            match guard.as_mut() {
                                                Some(service) => service.get_wake_word_opus(),
                                                None => None,
                                            }
                                        };
                                        let payload = match opus_data {
                                            Some(data) if !data.is_empty() => data,
                                            _ => break,
                                        };
                                        let packet = AudioStreamPacket {
                                            sample_rate: AUDIO_INPUT_SAMPLE_RATE as i32,
                                            frame_duration: OPUS_FRAME_DURATION_MS as i32,
                                            timestamp: 0,
                                            payload,
                                        };
                                        if let Err(e) = self.protocol.send_audio(&packet) {
                                            error!("Failed to send wake word audio: {:?}", e);
                                            break;
                                        }
                                    }
                                    // 4. 发送 listen/detect 消息，服务器将回应 TTS（如"我在呢"）
                                    if let Err(e) =
                                        self.protocol.send_wake_word_detected(&wake_word)
                                    {
                                        error!("Failed to send wake word detected: {:?}", e);
                                    }
                                    // 5. 进入聆听（C++: SetListeningMode(aec ? Realtime : AutoStop)）
                                    self.set_listening_mode(if self.aec_mode == AecMode::Off {
                                        ListeningMode::AutoStop
                                    } else {
                                        ListeningMode::Realtime
                                    });
                                }
                                DeviceState::Speaking => {
                                    // 说话过程中唤醒词打断（C++: AbortSpeaking(kAbortReasonWakeWordDetected)）
                                    self.abort_speaking(AbortReason::WakeWordDetected);
                                }
                                DeviceState::Activating => {
                                    self.set_device_state(DeviceState::Idle);
                                }
                                _ => {}
                            }
                        }
                        AppEvent::SpeakButtonClicked => {
                            info!("Boot button clicked! current state: {:?}", self.state);
                            if self.state == DeviceState::Starting
                                && !self.board.get_wifi_driver().is_connected().unwrap_or(false)
                            {
                                // TODO: 重置WiFi配置
                                // self.reset_wifi_configuration();
                            }
                            self.toggle_device_state();
                        }
                        AppEvent::VolumeButtonClicked => {
                            info!("Volume button clicked! current state: {:?}", self.state);
                            let mut volume = self
                                .board
                                .get_audio_codec()
                                .lock()
                                .unwrap()
                                .get_output_volume()?;

                            volume += 2;
                            if volume > 100 {
                                volume = 100;
                            }
                            match self
                                .board
                                .get_audio_codec()
                                .lock()
                                .unwrap()
                                .set_output_volume(volume)
                            {
                                Ok(_) => {
                                    info!("set output volumen OK!");
                                }
                                Err(err) => {
                                    error!("set_output_volume: {:?}", err);
                                }
                            };

                            self.board
                                .get_display()
                                .set_status(format!("当前音量：{}", volume).as_str());
                        }

                        AppEvent::VolumeButtonLongPressed => {
                            self.board
                                .get_audio_codec()
                                .lock()
                                .unwrap()
                                .set_output_volume(0)?;

                            self.board.get_display().set_status("当前音量:静音");
                        }

                        AppEvent::WebSocketConnected => {
                            // self.protocol.set_connected(true);
                            // info!("Connected,try to send hello message");
                            // if let Err(err) = self.protocol.send_hello_message() {
                            //     error!("Failed to send hello message: {:?}", err);
                            // }
                        }

                        AppEvent::WebSocketClosed => {
                            info!("WebSocketClosed, state={:?}", self.state);
                            // board.SetPowerSaveMode(true);
                            // Schedule([this]() {
                            //     auto display = Board::GetInstance().GetDisplay();
                            //     display->SetChatMessage("system", "");
                            //     SetDeviceState(kDeviceStateIdle);
                            // }); });
                            self.protocol.set_connected(false);
                            self.play_silence(); //如果设备在speaking，服务器突然关闭，这时设备会一直嗒嗒嗒地响，在这里加一段静音，可以解决这个问题。
                            self.set_device_state(DeviceState::Idle);
                        }
                        AppEvent::WebsocketTextMessageReceived(text) => {
                            info!("Received text message: {}", text);
                            match serde_json::from_str::<serde_json::Value>(&text) {
                                Ok(message) => {
                                    info!("成功解析json！");
                                    if let Some(message_type) = message["type"].as_str() {
                                        //             if (strcmp(type->valuestring, "tts") == 0) {
                                        //             auto state = cJSON_GetObjectItem(root, "state");
                                        //             if (strcmp(state->valuestring, "start") == 0) {
                                        //                 Schedule([this]() {
                                        //                     aborted_ = false;
                                        //                     if (device_state_ == kDeviceStateIdle || device_state_ == kDeviceStateListening) {
                                        //                         SetDeviceState(kDeviceStateSpeaking);
                                        //                     }
                                        //                 });
                                        //             } else if (strcmp(state->valuestring, "stop") == 0) {
                                        //                 Schedule([this]() {
                                        //                     background_task_->WaitForCompletion();
                                        //                     if (device_state_ == kDeviceStateSpeaking) {
                                        //                         if (listening_mode_ == kListeningModeManualStop) {
                                        //                             SetDeviceState(kDeviceStateIdle);
                                        //                         } else {
                                        //                             SetDeviceState(kDeviceStateListening);
                                        //                         }
                                        //                     }
                                        //                 });
                                        //             } else if (strcmp(state->valuestring, "sentence_start") == 0) {
                                        //                 auto text = cJSON_GetObjectItem(root, "text");
                                        //                 if (cJSON_IsString(text)) {
                                        //                     ESP_LOGI(TAG, "<< %s", text->valuestring);
                                        //                     Schedule([this, display, message = std::string(text->valuestring)]() {
                                        //                         display->SetChatMessage("assistant", message.c_str());
                                        //                     });
                                        //                 }
                                        //             }
                                        //         } else if (strcmp(type->valuestring, "stt") == 0) {
                                        //             auto text = cJSON_GetObjectItem(root, "text");
                                        //             if (cJSON_IsString(text)) {
                                        //                 ESP_LOGI(TAG, ">> %s", text->valuestring);
                                        //                 Schedule([this, display, message = std::string(text->valuestring)]() {
                                        //                     display->SetChatMessage("user", message.c_str());
                                        //                 });
                                        //             }
                                        //         } else if (strcmp(type->valuestring, "llm") == 0) {
                                        //             auto emotion = cJSON_GetObjectItem(root, "emotion");
                                        //             if (cJSON_IsString(emotion)) {
                                        //                 Schedule([this, display, emotion_str = std::string(emotion->valuestring)]() {
                                        //                     display->SetEmotion(emotion_str.c_str());
                                        //                 });
                                        //             }
                                        // #if CONFIG_IOT_PROTOCOL_MCP
                                        //         } else if (strcmp(type->valuestring, "mcp") == 0) {
                                        //             auto payload = cJSON_GetObjectItem(root, "payload");
                                        //             if (cJSON_IsObject(payload)) {
                                        //                 McpServer::GetInstance().ParseMessage(payload);
                                        //             }
                                        // #endif
                                        // #if CONFIG_IOT_PROTOCOL_XIAOZHI
                                        //         } else if (strcmp(type->valuestring, "iot") == 0) {
                                        //             auto commands = cJSON_GetObjectItem(root, "commands");
                                        //             if (cJSON_IsArray(commands)) {
                                        //                 auto& thing_manager = iot::ThingManager::GetInstance();
                                        //                 for (int i = 0; i < cJSON_GetArraySize(commands); ++i) {
                                        //                     auto command = cJSON_GetArrayItem(commands, i);
                                        //                     thing_manager.Invoke(command);
                                        //                 }
                                        //             }
                                        // #endif
                                        //         } else if (strcmp(type->valuestring, "system") == 0) {
                                        //             auto command = cJSON_GetObjectItem(root, "command");
                                        //             if (cJSON_IsString(command)) {
                                        //                 ESP_LOGI(TAG, "System command: %s", command->valuestring);
                                        //                 if (strcmp(command->valuestring, "reboot") == 0) {
                                        //                     // Do a reboot if user requests a OTA update
                                        //                     Schedule([this]() {
                                        //                         Reboot();
                                        //                     });
                                        //                 } else {
                                        //                     ESP_LOGW(TAG, "Unknown system command: %s", command->valuestring);
                                        //                 }
                                        //             }
                                        //         } else if (strcmp(type->valuestring, "alert") == 0) {
                                        //             auto status = cJSON_GetObjectItem(root, "status");
                                        //             auto message = cJSON_GetObjectItem(root, "message");
                                        //             auto emotion = cJSON_GetObjectItem(root, "emotion");
                                        //             if (cJSON_IsString(status) && cJSON_IsString(message) && cJSON_IsString(emotion)) {
                                        //                 Alert(status->valuestring, message->valuestring, emotion->valuestring, Lang::Sounds::P3_VIBRATION);
                                        //             } else {
                                        //                 ESP_LOGW(TAG, "Alert command requires status, message and emotion");
                                        //             }
                                        //         } else {
                                        //             ESP_LOGW(TAG, "Unknown message type: %s", type->valuestring);
                                        //         } });

                                        if message_type == "hello" {
                                            // self.protocol.set_connected(true);
                                        }

                                        if message_type == "tts" {
                                            if let Some(state) = message["state"].as_str() {
                                                if state == "start" {
                                                    self.shared_audio_state
                                                        .abort_speaking
                                                        .store(false, Ordering::SeqCst);
                                                    info!(
                                                        "处理文本消息开始: {} 当前状态: {:?}",
                                                        text, self.state
                                                    );
                                                    if self.state == DeviceState::Idle
                                                        || self.state == DeviceState::Listening
                                                    {
                                                        self.set_device_state(
                                                            DeviceState::Speaking,
                                                        );
                                                    }
                                                } else if state == "stop" {
                                                    info!(
                                                        "处理文本消息结束: {} 当前状态: {:?}",
                                                        text, self.state
                                                    );

                                                    // 对齐 C++ 原版 background_task_->WaitForCompletion()：
                                                    // tts stop 与最后几个 opus 包走同一 websocket，到达本分支时
                                                    // audio_decode_queue 里通常还积压着未解码的尾段包。
                                                    // 之前的做法是直接 clear() —— 尾段整个被扔掉，
                                                    // 表现为"话没说完就进 Listening"。正确做法：等 audio_loop
                                                    // 线程把队列消费完（队列为空且不在解码中），限时保护防卡死。
                                                    {
                                                        let deadline =
                                                            std::time::Instant::now()
                                                                + Duration::from_millis(3000);
                                                        loop {
                                                            let queue_empty = self
                                                                .shared_audio_state
                                                                .audio_decode_queue
                                                                .lock()
                                                                .unwrap()
                                                                .is_empty();
                                                            let idle = !self
                                                                .shared_audio_state
                                                                .busy_decoding_audio
                                                                .load(Ordering::SeqCst);
                                                            if queue_empty && idle {
                                                                break;
                                                            }
                                                            if std::time::Instant::now()
                                                                >= deadline
                                                            {
                                                                warn!(
                                                                    "TTS drain timeout, dropping tail packets"
                                                                );
                                                                self.shared_audio_state
                                                                    .audio_decode_queue
                                                                    .lock()
                                                                    .unwrap()
                                                                    .clear();
                                                                break;
                                                            }
                                                            std::thread::sleep(
                                                                Duration::from_millis(20),
                                                            );
                                                        }
                                                    }

                                                    self.play_silence();

                                                    if self.state == DeviceState::Speaking {
                                                        if self.listening_mode
                                                            == ListeningMode::Manual
                                                        {
                                                            self.set_device_state(
                                                                DeviceState::Idle,
                                                            );
                                                        } else {
                                                            self.set_device_state(
                                                                DeviceState::Listening,
                                                            );
                                                        }
                                                    } else {
                                                        warn!(
                                                            "tts stop 被忽略：当前状态={:?}（预期 Speaking）",
                                                            self.state
                                                        );
                                                    }
                                                }
                                                // TODO:: 处理其它文本
                                            }
                                        }
                                    }
                                    info!("处理文本消息结束: {}", text);
                                }
                                Err(e) => {
                                    error!("Failed to parse JSON: {:?}", e);
                                }
                            }

                            // let message: serde_json::Value = serde_json::from_str(&text).unwrap();
                        }
                        AppEvent::SendAudioEvent => {
                            // info!("XzEvent::SendAudioEvent");
                            let packets = {
                                let mut queue = audio_packet_send_queue_arc.lock().unwrap();
                                // std::mem::take 会把 queue 换成默认值（空），并把原来的值返回
                                // 这完全等同于 C++ 的 std::move
                                std::mem::take(&mut *queue)
                            };

                            // 此时锁已经释放了
                            for packet in packets {
                                // info!("send audio packet using protocol!");
                                self.protocol.send_audio(&packet)?;
                            }
                        }
                        AppEvent::AudioPacketReceived(audio_stream_packet) => {
                            if self.audio_test_mode {
                                // 在主线程中处理音频数据包
                                info!("received audio data, play_opus_audio");
                                codec_for_opus_player
                                    .lock()
                                    .unwrap()
                                    .play_opus(
                                        opus_decoder,
                                        audio_stream_packet.payload.as_slice(),
                                        &mut shared_pcm_buffer,
                                    )
                                    .unwrap();
                            } else {
                                // // 处理从服务器端接收到的音频数据包
                                // info!(
                                //     "XzEvent::AudioPacketReceived - 从服务器端接收到的音频数据包 当前状态: {:?}",
                                //     self.state
                                // );
                                if self.state == DeviceState::Speaking
                                    && audio_packet_send_queue_arc.lock().unwrap().len()
                                        < MAX_AUDIO_PACKETS_IN_QUEUE
                                {
                                    // 在子线程中处理音频解码
                                    let mut audio_decode_queue =
                                        self.shared_audio_state.audio_decode_queue.lock().unwrap();
                                    audio_decode_queue.push_back(audio_stream_packet);

                                    // match self
                                    //     .decode_task_sender
                                    //     .clone()
                                    //     .send(AppEvent::AudioPacketReceived(audio_stream_packet))
                                    // {
                                    //     Ok(_) => {
                                    //         // info!("send audio decode event ok");
                                    //     }
                                    //     Err(e) => {
                                    //         error!("send audio decode event error: {:?}", e);
                                    //     }
                                    // }

                                    // // 在主线程中处理音频数据包
                                    // match codec_for_opus_player.lock().unwrap().play_opus(
                                    //     opus_decoder,
                                    //     audio_stream_packet.payload.as_slice(),
                                    //     &mut shared_pcm_buffer,
                                    // ) {
                                    //     Ok(()) => {
                                    //         // info!("codec play_opus ok");
                                    //     }
                                    //     Err(e) => {
                                    //         error!("codec::play_opus error: {:?}", e);
                                    //     }
                                    // }
                                }
                            }
                        }

                        AppEvent::AddAudioPacketToQueue(packet) => {
                            // info!("XzEvent::AddAudioPacketToQueue: add audio packet to queue");
                            // 把编码后的音频包添加待发送队列
                            let audio_packet_queue = Arc::clone(&audio_packet_send_queue_arc);
                            let mut queue = audio_packet_queue.lock().unwrap();

                            // --- 核心逻辑在这里 ---
                            // 2. 检查队列是否已满
                            if queue.len() >= MAX_AUDIO_PACKETS_IN_QUEUE {
                                warn!("Too many audio packets in queue, drop the newest packet");
                                continue;
                            }

                            // 4. 将新元素推入队列的尾部
                            queue.push_back(packet.clone());

                            if !self.audio_test_mode {
                                // 5. 唤醒音频发送线程把音频发送到服务器端
                                self.inner_sender
                                    .clone()
                                    .send(AppEvent::SendAudioEvent)
                                    .unwrap();
                            } else {
                                // //如果是音频测试模式，则不把音频数据发送给服务器
                                // let mut audio_decode_queue =
                                //     self.audio_decode_queue.lock().unwrap();
                                // audio_decode_queue.push_back(packet);
                                // //开始本地解码
                                // self.decode_task_sender
                                //     .send(XzEvent::AudioDecodeEvent)
                                //     .unwrap();
                            }
                        }

                        AppEvent::ProtocolNetworkError(err) => {
                            self.play_silence(); //如果设备在speaking，服务器突然关闭，这时设备会一直嗒嗒嗒地响，在这里加一段静音，可以解决这个问题。
                            self.set_device_state(DeviceState::Idle);
                            error!("ProtocolNetworkError: {:?}", err);
                        }

                        AppEvent::PlayAudioAlert(message) => {
                            self.audio_alert(&message);
                        }

                        AppEvent::RefreshWifiSignal => {
                            if let Ok(rssi) = self.board.get_wifi_driver().get_rssi() {
                                self.board.get_display().show_wifi_signal(rssi);
                            }
                        }

                        _ => {
                            info!("Received unhandled event: {:?}", event);
                        }
                    }
                }
                Err(_) => {
                    info!("Event channel closed, exiting event loop");
                }
            }
        }
        // info!("application start 函数返回！");
    }

    pub fn read_audio(
        &mut self,
        mut i2s_driver: MutexGuard<'_, I2sDriver<'_, I2sBiDir>>,
        mut buffer: std::sync::MutexGuard<'_, VecDeque<i16>>,
    ) -> Result<(), Error> {
        // 读取音频数据
        self.state = DeviceState::Listening;
        info!("Reading audio...");
        Ok(())
    }

    // private methods
    fn set_device_state(&mut self, state: DeviceState) {
        if self.state == state {
            return;
        }

        let previous_state = self.state.clone();
        self.state = state;

        match self.state {
            DeviceState::Idle => {
                info!(
                    "Device state changed from {:?} to {:?}",
                    previous_state, self.state
                );
                // display->SetStatus(Lang::Strings::STANDBY);
                let display: &mut LcdSt7789 = self.board.get_display();
                display.set_status("空闲状态");
                // display->SetEmotion("neutral");
                // audio_processor_->Stop();
                // wake_word_->StartDetection();
                self.audio_processor.lock().unwrap().stop();
                // 回到空闲状态，恢复唤醒词检测
                if let Some(service) = self.wake_word_service.lock().unwrap().as_mut() {
                    service.start_detection();
                }
            }
            DeviceState::Activating => {
                info!(
                    "Device state changed from {:?} to {:?}",
                    previous_state, self.state
                );
                self.board.get_display().set_status("激活中");
            }
            DeviceState::WifiConfiguring => {
                info!(
                    "Device state changed from {:?} to {:?}",
                    previous_state, self.state
                );
                self.board.get_display().set_status("配置网络");
            }
            DeviceState::Connecting => {
                info!(
                    "Device state changed from {:?} to {:?}",
                    previous_state, self.state
                );
                self.board.get_display().set_status("连接中");
            }
            // DeviceState::DeviceStateAudioTesting => todo!(),
            DeviceState::Speaking => {
                info!(
                    "Device state changed from {:?} to {:?}",
                    previous_state, self.state
                );

                if self.listening_mode != ListeningMode::Realtime {
                    self.audio_processor.lock().unwrap().stop();
                    // 对应 C++ #if CONFIG_USE_AFE_WAKE_WORD：
                    // 说话时继续唤醒检测，支持说话过程中唤醒词打断
                    if let Some(service) = self.wake_word_service.lock().unwrap().as_mut() {
                        service.start_detection();
                    }
                }
                self.reset_decoder();
                self.board.get_display().set_status("正在说话");
            }
            DeviceState::Listening => {
                info!(
                    "Listening state changed from {:?} to {:?}",
                    previous_state, self.state
                );

                if !self.audio_processor.lock().unwrap().is_running() {
                    if !self.protocol.is_connected() {
                        self.protocol.open_audio_channel().unwrap();

                        // self.wait_for_audio_channel_opened();
                    }

                    self.protocol
                        .send_start_linstening(self.listening_mode.clone())
                        .unwrap();
                    // TODO::
                    // if (previous_state == kDeviceStateSpeaking) {
                    //     audio_decode_queue_.clear();
                    //     audio_decode_cv_.notify_all();
                    //     // FIXME: Wait for the speaker to empty the buffer
                    //     vTaskDelay(pdMS_TO_TICKS(120));
                    // }
                    // opus_encoder_->ResetState();
                    // audio_processor_->Start(); //启动音频处理器。
                    // wake_word_->StopDetection();
                    if let Some(service) = self.wake_word_service.lock().unwrap().as_mut() {
                        service.stop_detection();
                    }
                    self.opus_encoder.lock().unwrap().reset_state();
                    self.audio_processor.lock().unwrap().start();
                    self.board.get_display().set_status("正在聆听");
                }
            }
            DeviceState::Starting => {
                info!(
                    "Starting state changed from {:?} to {:?}",
                    previous_state, self.state
                );
                self.board.get_display().set_status("启动中");
            }
            _ => {}
        }
    }

    fn set_listening_mode(&mut self, mode: ListeningMode) {
        self.listening_mode = mode;
        self.set_device_state(DeviceState::Listening);
    }
    fn toggle_device_state(&mut self) {
        match self.state {
            DeviceState::Activating => {
                self.set_device_state(DeviceState::Idle);
                return;
            }
            DeviceState::WifiConfiguring => {
                // self.enter_audio_testing_mode();
                return;
            }
            // DeviceState::AudioTesting => {
            //     // self.exit_audio_testing_mode();
            //     return;
            // }
            //     _ => {}
            // }

            // // if self.protocol.is_none() {
            // //     error!("Protocol not initialized");
            // //     return;
            // // }

            // match self.state {
            DeviceState::Idle => {
                // if !self.protocol.is_audio_channel_opened() {
                //     self.set_device_state(DeviceState::Connecting);
                //     if !self.protocol.open_audio_channel().unwrap_or(false) {
                //         return;
                //     }
                // }

                self.set_listening_mode(if self.aec_mode == AecMode::Off {
                    ListeningMode::AutoStop
                } else {
                    ListeningMode::Realtime
                });
            }
            DeviceState::Speaking => {
                self.abort_speaking(AbortReason::None);
            }
            DeviceState::Listening => {
                info!("DeviceState::Listening - Closing audio channel...");
                {
                    let mut audio_processor = self.audio_processor.lock().unwrap();
                    audio_processor.stop();
                }

                self.stop_listening();

                // 对齐 C++ ToggleChatState: Listening -> CloseAudioChannel()
                // Idle 是纯本地离线状态（唤醒词检测完全不依赖服务器连接），
                // 下次唤醒词触发时再重新建连（wake handler 中的 open_audio_channel 路径）
                if let Err(e) = self.protocol.close_audio_channel() {
                    error!("Failed to close_audio_channel: {:?}", e);
                }
            }
            _ => {}
        }

        // if (device_state_ == kDeviceStateActivating) {
        //     SetDeviceState(kDeviceStateIdle);
        //     return;
        // } else if (device_state_ == kDeviceStateWifiConfiguring) {
        //     EnterAudioTestingMode();
        //     return;
        // } else if (device_state_ == kDeviceStateAudioTesting) {
        //     ExitAudioTestingMode();
        //     return;
        // }

        // if (!protocol_) {
        //     ESP_LOGE(TAG, "Protocol not initialized");
        //     return;
        // }

        // if (device_state_ == kDeviceStateIdle) {
        //     Schedule([this]() {
        //         if (!protocol_->IsAudioChannelOpened()) {
        //             SetDeviceState(kDeviceStateConnecting);
        //             if (!protocol_->OpenAudioChannel()) {
        //                 return;
        //             }
        //         }

        //         SetListeningMode(aec_mode_ == kAecOff ? kListeningModeAutoStop : kListeningModeRealtime);
        //     });
        // } else if (device_state_ == kDeviceStateSpeaking) {
        //     Schedule([this]() {
        //         AbortSpeaking(kAbortReasonNone);
        //     });
        // } else if (device_state_ == kDeviceStateListening) {
        //     Schedule([this]() {
        //         protocol_->CloseAudioChannel();
        //     });
        // }
    }

    fn start_output_audio(&mut self) {
        let pcm_tx = self.inner_pcm_tx.clone();
        if let Some(rx) = self.decode_task_receiver.take() {
            run_audio_decode_task(rx, pcm_tx);
        } else {
            println!("Receiver already taken!");
        }
    }

    fn stop_listening(&mut self) {
        //     if (device_state_ == kDeviceStateAudioTesting)
        // {
        //     ExitAudioTestingMode();
        //     return;
        // }

        // const std::array<int, 3> valid_states = {
        //     kDeviceStateListening,
        //     kDeviceStateSpeaking,
        //     kDeviceStateIdle,
        // };
        // // If not valid, do nothing
        // if (std::find(valid_states.begin(), valid_states.end(), device_state_) == valid_states.end())
        // {
        //     return;
        // }

        // Schedule([this]()
        //          {
        //     if (device_state_ == kDeviceStateListening) {
        //         protocol_->SendStopListening();
        //         SetDeviceState(kDeviceStateIdle);
        //     } });

        let valid_stats = vec![
            DeviceState::Listening,
            DeviceState::Speaking,
            DeviceState::Idle,
        ];

        if !valid_stats.contains(&self.state) {
            return;
        }

        if self.state == DeviceState::Listening {
            self.protocol.send_stop_listening().unwrap();
            self.set_device_state(DeviceState::Idle);
        }
    }

    fn reset_decoder(&mut self) {
        // std::lock_guard<std::mutex> lock(mutex_);
        // opus_decoder_->ResetState();
        // audio_decode_queue_.clear();
        // audio_decode_cv_.notify_all();
        // last_output_time_ = std::chrono::steady_clock::now();
        // auto codec = Board::GetInstance().GetAudioCodec();
        // codec->EnableOutput(true);

        self.opus_decoder.lock().unwrap().reset_state();
        self.shared_audio_state
            .audio_decode_queue
            .lock()
            .unwrap()
            .clear();
        // self.audio_decode_cv.lock().unwrap().notify_all();
        // self.last_output_time = Instant::now();
        self.board
            .get_audio_codec()
            .lock()
            .unwrap()
            .enable_output(true)
            .unwrap();
    }

    fn play_p3_audio(&mut self, filename: &str) {
        // const P3_DATA: &'static [u8] = include_bytes!("../assets/zh-CN/wificonfig.p3");
        // const P3_DATA: &'static [u8] = include_bytes!(p3_file);

        // info!(
        //     "Embedded p3 data size: {} bytes. Starting playback...",
        //     P3_DATA.len()
        // );

        let p3_data = match filename {
            "wificonfig" => Some(include_bytes!("../assets/zh-CN/wificonfig.p3").to_vec()),
            "welcome" => Some(include_bytes!("../assets/zh-CN/welcome.p3").to_vec()),
            "success" => Some(include_bytes!("../assets/common/success.p3").to_vec()),
            _ => None,
        };

        if let Some(p3_data) = p3_data {
            self.play_p3_data(p3_data);
        } else {
            error!("Failed to find p3 data for file: {}", filename);
            return;
        }
    }

    fn play_p3_data(&mut self, p3_data: Vec<u8>) {
        const CHUNK_SIZE: usize = 4096;

        // info!("Starting playback in chunks of {} bytes...", CHUNK_SIZE);

        if p3_data.len() < 4 {
            error!("P3 data is too small to be valid.");
            return;
        }

        let p3_data_len = p3_data.len();
        // info!("P3 data length: {} bytes", p3_data_len);

        let sample_rate = AUDIO_INPUT_SAMPLE_RATE as i32; //# 采样率固定为16000Hz
        let channels = 1; //# 单声道
        let mut opus_decoder = OpusAudioDecoder::new(
            sample_rate,
            channels,
            OPUS_FRAME_DURATION_MS.try_into().unwrap(),
        )
        .unwrap();

        let mut offset = 0;

        while offset < p3_data_len {
            let len: [u8; 2] = p3_data[offset + 2..offset + 4].try_into().unwrap();
            let frame_len = u16::from_be_bytes(len) as usize;

            let opus_data = &p3_data[(offset + 4)..(offset + 4 + frame_len)];
            offset += 4 + frame_len;
            // info!("offset {} bytes...", offset);

            // decoder = decoder.decode(sample_rate, channels);
            let decode_result = opus_decoder.decode(opus_data);

            match decode_result {
                Ok(pcm_data) => {
                    //因为 p3文件是单声道的，而我们的 I2S 配置是双声道的，所以需要将单声道数据转换成双声道数据。
                    let pcm_mono_data_len = pcm_data.len();

                    let mut pcm_stereo_buffer = vec![0i16; pcm_mono_data_len * 2];

                    // 2. 遍历单声道样本，并复制到立体声缓冲区的左右声道
                    for i in 0..pcm_mono_data_len {
                        let sample = pcm_data[i];
                        pcm_stereo_buffer[i * 2] = sample; // 左声道
                        pcm_stereo_buffer[i * 2 + 1] = sample; // 右声道
                    }

                    let pcm_stereo_bytes: &[u8] = unsafe {
                        core::slice::from_raw_parts(
                            pcm_stereo_buffer.as_ptr() as *const u8,
                            pcm_stereo_buffer.len() * std::mem::size_of::<i16>(),
                        )
                    };

                    // 如果p3是双声道的，或者使用了单声道的 I2S 配置，我们就可以直接使用 decode 后的音频数据。
                    // // 1. 首先，获取一个指向有效数据的切片
                    // let pcm_slice: &[i16] = &pcm_data;
                    // // 2. 使用unsafe块来进行零成本的类型转换
                    // let pcm_bytes: &[u8] = unsafe {
                    //     // a. 获取i16切片的裸指针和长度（以i16为单位）
                    //     let ptr = pcm_slice.as_ptr();
                    //     let len_in_i16 = pcm_slice.len();
                    //     // b. 使用`core::slice::from_raw_parts`来创建一个新的字节切片
                    //     //    - 将i16指针强制转换成u8指针
                    //     //    - 将长度（以i16为单位）乘以每个i16的字节数（2），得到总的字节长度
                    //     core::slice::from_raw_parts(
                    //         ptr as *const u8,
                    //         len_in_i16 * std::mem::size_of::<i16>(),
                    //     )
                    // };

                    let pcm_sender = self.inner_pcm_tx.clone();

                    // match pcm_sender.send(vec_pcm_data) {
                    //     Ok(_) => {}
                    //     Err(err) => {
                    //         error!("Failed to send pcm data: {:?}", err);
                    //     }
                    // }

                    // // 3. 使用 .chunks() 方法将整个PCM数据切分成多个小块
                    for chunk in pcm_stereo_bytes.chunks(CHUNK_SIZE) {
                        match pcm_sender.send(chunk.to_vec()) {
                            Ok(_) => {}
                            Err(err) => {
                                error!("Failed to send pcm data: {:?}", err);
                            }
                        }
                    }
                }
                Err(e) => {
                    info!("Opus decode error: {:?}", e);
                    return;
                }
            }
        }
    }

    fn abort_speaking(&mut self, reason: AbortReason) {
        self.shared_audio_state
            .abort_speaking
            .store(true, Ordering::SeqCst);

        if let Err(err) = self.protocol.send_abort_speaking(reason) {
            error!("Failed to send abort speaking: {:?}", err);
        }
    }

    fn play_silence(&mut self) {
        // 建议 buffer 大小为 DMA buffer 的一到两倍，确保能填满硬件残留
        const SILENCE_BUFFER: [u8; 2048] = [0u8; 2048];

        let pcm_player_codec = self.board.get_audio_codec().clone();
        // 喂入几帧静音数据，覆盖掉 DMA 里剩下的残留
        for _ in 0..5 {
            pcm_player_codec
                .lock()
                .unwrap()
                .output_data(&SILENCE_BUFFER)
                .unwrap();
        }
    }

    fn wait_for_audio_channel_opened(&mut self) {
        info!("waiting for audio channel to be opened");
        let mut timeout = 10 * 1000;
        loop {
            if self.protocol.is_connected() {
                break;
            } else {
                let sleep_duration = 10;
                thread::sleep(std::time::Duration::from_millis(sleep_duration));
                timeout -= sleep_duration;
                if timeout <= 0 {
                    info!("wait_for_audio_channel_opened --- timeout!");
                    break;
                }
            }
        }
        info!("wait_for_audio_channel_opened --- audio channel opened");
    }
}

fn audio_loop(
    audio_codec: Arc<Mutex<dyn AudioCodec>>,
    audio_processor: Arc<Mutex<dyn AudioProcessor>>,
    share_audio_state: Arc<SharedAudioState>,
    inner_pcm_tx: SyncSender<Vec<u8>>,
    wake_word_service: Arc<Mutex<Option<WakeWordService>>>,
) {
    // let mut codec = audio_codec.lock().unwrap();
    // codec.set_output_volume(50);
    // let codec_arc = Arc::clone(&audio_codec);
    // let codec_arc1 = Arc::clone(&audio_codec);
    let audio_processor_arc = Arc::clone(&audio_processor);

    let feed_size = audio_processor.lock().unwrap().get_feed_size();
    // info!("application: feed_size: {}", feed_size);
    // const READ_CHUNK_SIZE: usize = 1024;
    let mut read_buffer = vec![0u8; feed_size];

    // 唤醒词检测的 feed 缓冲（get_feed_size 返回 i16 样本数，乘 2 转为字节数）
    let wake_feed_size = {
        let guard = wake_word_service.lock().unwrap();
        guard.as_ref().map(|s| s.get_feed_size()).unwrap_or(0)
    };
    let mut wake_read_buffer = vec![0u8; wake_feed_size.saturating_mul(2)];

    let mut shared_decode_buffer: Vec<i16> = Vec::with_capacity(4096);
    // let mut pcm_buffer: Vec<u8> = Vec::with_capacity(38400);

    let sample_rate = AUDIO_INPUT_SAMPLE_RATE as i32; //# 采样率固定为16000Hz
    let channels = 2; //# 单声道
    let mut opus_decoder = OpusAudioDecoder::new(
        sample_rate,
        channels,
        OPUS_FRAME_DURATION_MS.try_into().unwrap(),
    )
    .unwrap();

    // let mut cache_packet_count: i32 = 0;

    loop {
        let pcm_tx = inner_pcm_tx.clone();
        let audio_state = share_audio_state.clone();
        start_audio_input(
            Arc::clone(&audio_codec),
            audio_processor_arc.clone(),
            &wake_word_service,
            &mut read_buffer,
            &mut wake_read_buffer,
        );

        let codec_arc = Arc::clone(&audio_codec);
        if codec_arc.lock().unwrap().output_enabled() {
            start_audio_output(
                // codec_arc,
                // audio_processor_arc.clone(),
                audio_state,
                &mut opus_decoder,
                &mut shared_decode_buffer,
                // &mut pcm_buffer,
                pcm_tx,
                // &mut cache_packet_count,
            );
        } else {
            info!("application: output_enabled: false");
        }

        // thread::sleep(Duration::from_millis(10));
    }
}

fn start_audio_output(
    // codec_arc: Arc<Mutex<dyn AudioCodec + 'static>>,
    // audio_processor: Arc<Mutex<dyn AudioProcessor + 'static>>,
    share_audio_state: Arc<SharedAudioState>,
    opus_decoder: &mut OpusAudioDecoder,
    decode_buffer: &mut Vec<i16>,
    // pcm_buffer: &mut Vec<u8>,
    pcm_sender: SyncSender<Vec<u8>>,
) {
    // info!("application: start_audio_output");

    // If app is busy decoding audio, return
    if share_audio_state.busy_decoding_audio.load(Ordering::SeqCst) {
        // info!("application: busy_decoding_audio: true");
        return;
    }

    if share_audio_state
        .audio_decode_queue
        .lock()
        .unwrap()
        .is_empty()
    {
        // info!("application: audio_decode_queue is empty");
        return;
    }

    // let packet = share_audio_state
    //     .audio_decode_queue
    //     .lock()
    //     .unwrap()
    //     .pop_front();

    let packets = {
        let mut queue = share_audio_state.audio_decode_queue.lock().unwrap();
        // std::mem::take 会把 queue 换成默认值（空），并把原来的值返回
        // 这完全等同于 C++ 的 std::move
        std::mem::take(&mut *queue)
    };
    for packet in packets {
        if share_audio_state.abort_speaking.load(Ordering::SeqCst) {
            info!("application: abort_speaking: true");
            return;
        }

        // if let Some(packet) = packet {
        share_audio_state
            .busy_decoding_audio
            .store(true, Ordering::SeqCst);
        // info!("application: got packet from audio_decode_queue");
        match decode_opus_audio(
            // codec.clone(),
            opus_decoder,
            packet.payload,
            decode_buffer,
        ) {
            Ok(pcm_data) => {
                match pcm_sender.send(pcm_data) {
                    Ok(_) => {
                        // info!("Send pcm data success.");
                    }
                    Err(e) => {
                        error!("Send decoded opus data(pcm data) error: {:?}", e);
                    }
                }
            }
            Err(e) => {
                error!("Failed to decode audio: {}", e);
            }
        }
        share_audio_state
            .busy_decoding_audio
            .store(false, Ordering::SeqCst);
    }
}

fn start_audio_input(
    codec: Arc<Mutex<dyn AudioCodec + 'static>>,
    audio_processor: Arc<Mutex<dyn AudioProcessor + 'static>>,
    wake_word_service: &Arc<Mutex<Option<WakeWordService>>>,
    mut read_buffer: &mut Vec<u8>,
    mut wake_read_buffer: &mut Vec<u8>,
) {
    // 对齐 C++ OnAudioInput：喂料路径上不做任何 sleep，全速读取，
    // 仅当 wake 与 processor 都未运行时（函数末尾）才 delay 半帧。
    // 前导 sleep 会导致消费速率低于生产速率，DMA 积压溢出、音频断续。
    // if (audio_processor_->IsRunning())
    // {
    //     std::vector<int16_t> data;
    //     int samples = audio_processor_->GetFeedSize();
    //     if (samples > 0)
    //     {
    //         if (ReadAudio(data, 16000, samples))
    //         {
    //             audio_processor_->Feed(data);
    //             return;
    //         }
    //     }
    // }

    // if audio_processor.lock().unwrap().is_running() {
    //     let samples = audio_processor.lock().unwrap().get_feed_size();
    //     let codec_arc = Arc::clone(&codec);

    //     if samples > 0 {
    //         let bytes_read = codec_arc
    //             .lock()
    //             .unwrap()
    //             .read_audio_data(&mut read_buffer)
    //             .unwrap();

    //         // 因为录音数据是8位PCM数据，opus_encoder 需要16位的 Vec,所以需转换下。
    //         let bytes_to_i16_result = bytes_to_i16_slice(&read_buffer[..bytes_read]).unwrap();
    //         info!("application: feed data to audio processor");
    //         audio_processor.lock().unwrap().feed(&bytes_to_i16_result);
    //     }
    // }

    // 1. 获取一次锁，检查状态并获取大小
    // 使用代码块 {} 限制锁的范围，确保尽快释放
    let (is_running, feed_size) = {
        let processor = audio_processor.lock().unwrap();
        (processor.is_running(), processor.get_feed_size())
    };

    // info!(
    //     "application: is_running: {}, feed_size: {}",
    //     is_running, feed_size
    // );

    // 2. 唤醒词检测优先：检测运行时把 codec 数据喂给 WakeWordService
    let (wake_running, wake_feed_size) = {
        let guard = wake_word_service.lock().unwrap();
        match guard.as_ref() {
            Some(service) => (service.is_detection_running(), service.get_feed_size()),
            None => (false, 0),
        }
    };

    if wake_running && wake_feed_size > 0 {
        // 唤醒喂料：codec 输出 codec_channels 声道交错数据；
        // AFE 需要的声道数可能不同（诊断模式下 AFE 只要 1 声道 MIC1）。
        let codec_channels = {
            let codec = codec.lock().unwrap();
            codec.input_channels().max(1) as usize
        };
        let afe_channels = {
            let guard = wake_word_service.lock().unwrap();
            match guard.as_ref() {
                Some(service) => service.input_channels().max(1),
                None => codec_channels,
            }
        };
        // wake_feed_size = AFE 每次所需样本数（含其全部声道）
        // 对应交错帧数 = wake_feed_size / afe_channels
        let frames_per_feed = wake_feed_size / afe_channels;
        let need_bytes = frames_per_feed * codec_channels * 2;
        if wake_read_buffer.len() < need_bytes {
            wake_read_buffer.resize(need_bytes, 0);
        }

        let bytes_read = match codec.lock().unwrap().read_audio_data(&mut wake_read_buffer) {
            Ok(bytes_read) => bytes_read,
            Err(e) => {
                error!("application: read_audio_data error: {:?}", e);
                0
            }
        };

        if bytes_read > 0 {
            match bytes_to_i16_slice(&wake_read_buffer[..bytes_read]) {
                Ok(samples) => {
                    // 分声道心跳探针：ch0 = MIC1（AFE 实际消费的麦克风），
                    // ch1 = 名义参考声道（MR 模式时被 AEC 当回采参考）。
                    // 判读：平时说话 ch1 大幅跟随 ch0 => ch1 是第二个麦克风，
                    // AEC 在抵消人声（本次诊断的假设）；仅设备出声时 ch1 大
                    // => ch1 是真实回采参考；ch1 恒为 0 => 参考悬空。
                    static WAKE_FEED_COUNT: AtomicU32 = AtomicU32::new(0);
                    let feeds = WAKE_FEED_COUNT.fetch_add(1, Ordering::Relaxed);
                    if feeds % 50 == 0 {
                        let usable = samples.len() / codec_channels * codec_channels;
                        let (mut max0, mut max1) = (0u16, 0u16);
                        for frame in samples[..usable].chunks(codec_channels) {
                            let a0 = frame[0].unsigned_abs();
                            if a0 > max0 {
                                max0 = a0;
                            }
                            if codec_channels > 1 {
                                let a1 = frame[1].unsigned_abs();
                                if a1 > max1 {
                                    max1 = a1;
                                }
                            }
                        }
                        let zeros = samples.iter().filter(|&&s| s == 0).count();
                        info!(
                            "wake feed alive: total {} feeds, ch0(MIC1) max_abs={}, ch1(ref?) max_abs={}, zeros={}/{}",
                            feeds + 1,
                            max0,
                            max1,
                            zeros,
                            samples.len()
                        );
                    }
                    // 按需去交错：诊断模式（afe_channels=1 < codec_channels）只取 ch0
                    let mono_buffer: Vec<i16>;
                    let feed_slice: &[i16] = if afe_channels < codec_channels {
                        let usable = samples.len() / codec_channels * codec_channels;
                        mono_buffer = samples[..usable]
                            .chunks(codec_channels)
                            .map(|frame| frame[0])
                            .collect();
                        &mono_buffer
                    } else {
                        samples
                    };
                    if feed_slice.len() >= wake_feed_size {
                        let mut guard = wake_word_service.lock().unwrap();
                        if let Some(service) = guard.as_mut() {
                            let _ = service.feed(&feed_slice[..wake_feed_size]);
                        }
                    } else {
                        warn!(
                            "wake feed underflow: got {} samples, need {}",
                            feed_slice.len(),
                            wake_feed_size
                        );
                    }
                }
                Err(_) => {
                    warn!("Wake word audio bytes not i16-aligned: {} bytes", bytes_read);
                }
            }
        } else {
            warn!("Wake word codec read returned 0 bytes (input pipeline issue?)");
        }
        return;
    }

    if is_running && feed_size > 0 {
        // let start = Instant::now();
        // 2. 读取音频 (耗时操作，不要持有 processor 的锁)
        // read_buffer 需要扩容以容纳数据
        // if read_buffer.len() < feed_size * 2 {
        //     // 假设是 i16，需要 2 倍字节
        //     read_buffer.resize(feed_size * 2, 0);
        // }
        // read_buffer.resize(1024, 0);

        let bytes_read = match codec.lock().unwrap().read_audio_data(&mut read_buffer) {
            Ok(bytes_read) => bytes_read,
            Err(e) => {
                error!("application: read_audio_data error: {:?}", e);
                0
            }
        };

        // info!("从codec读取音频数据的bytes_read = {}", bytes_read);

        // let duration = start.elapsed();
        // info!("从codec读取音频数据 耗时: {:?}", duration);

        if bytes_read > 0 {
            let audio_data = &read_buffer[..bytes_read];

            // info!("从es7210中读取的音频内容: {:?} ", audio_data);

            // let audio_data_md5 = calc_md5_builtin(audio_data);
            // info!(
            //     "音频数据： Feed 前 -  md5: {} - 内容: {:?} ",
            //     audio_data_md5, audio_data
            // );
            // let start = Instant::now();
            let bytes_to_i16_result = bytes_to_i16_slice(&audio_data).unwrap();

            // // 1. 创建一个干净的单声道缓冲区
            // // 容量是原来的一半
            // let mut mono_samples = Vec::with_capacity(bytes_to_i16_result.len() / 2);

            // // 2. 剔除那些全是 0 的奇数通道 (索引 1, 3, 5...)
            // // 只保留有声音的通道 0 (索引 0, 2, 4...)
            // for i in (0..bytes_to_i16_result.len()).step_by(2) {
            //     mono_samples.push(bytes_to_i16_result[i]);
            // }

            // let duration = start.elapsed();
            // info!("数据转换 耗时: {:?}", duration);
            // info!("application: feed data to audio processor");

            // info!("真正喂进去的 i16 长度: {}", bytes_to_i16_result.len());

            // // 打印最大振幅，以调试es7210的输出音量
            // let max_val = mono_samples
            //     .iter()
            //     .map(|&x| if x == i16::MIN { 32767 } else { x.abs() })
            //     .max()
            //     .unwrap_or(0);
            // info!(
            //     "首几个样本: [{}, {}, {}, {}]",
            //     mono_samples[0], mono_samples[1], mono_samples[2], mono_samples[3]
            // );
            // info!("音频数据最大振幅: {}", max_val);
            // if max_val >= 10000 && max_val <= 28000 {
            //     info!(" read data from codec es7210, bytes_read: {}", bytes_read);
            //     info!("检测到合理的音频数据: {}", max_val);
            // }

            // // 3. 再次获取锁进行 feed从es7210中读取的音频内容
            // // 此时 codec 的锁已经释放了，避免交叉死锁
            // let start = Instant::now();

            // info!(
            //     "首几个样本: [{}, {}, {}, {}]",
            //     bytes_to_i16_result[0],
            //     bytes_to_i16_result[1],
            //     bytes_to_i16_result[2],
            //     bytes_to_i16_result[3]
            // );
            // info!("Feed 数据 前 - 内容: {} ", bytes_to_i16_result.len());
            audio_processor.lock().unwrap().feed(&bytes_to_i16_result);
            // let duration = start.elapsed();
            // info!("Feed 数据 耗时: {:?}", duration);
        } else {
            info!("bytes_read is 0, 不进行feed");
        }

        return;
    }

    // 对齐 C++ OnAudioInput 末尾的 vTaskDelay(OPUS_FRAME_DURATION_MS / 2)：
    // 仅在 wake 检测与音频处理器都未运行（空闲）时才休眠半帧
    thread::sleep(Duration::from_millis((OPUS_FRAME_DURATION_MS / 2) as u64));
}

fn decode_opus_audio1(
    codec: Arc<Mutex<dyn AudioCodec + 'static>>,
    opus_decoder: &mut OpusAudioDecoder,
    // mut i2s_driver: MutexGuard<'_, I2sDriver<'_, I2sBiDir>>,
    opus_data: Vec<u8>,
    pcm_buffer: &mut Vec<i16>,
) {
    // let sample_rate = 16000; //# 采样率固定为16000Hz
    let channels = 2; //# 双声道
                      // let channels = 1; //# 双声道

    let decode_result = opus_decoder.decode(&opus_data);

    // let mut decoder = Box::new(OpusAudioDecoder::new(sample_rate, channels).unwrap());
    // let decode_result = decoder.decode(&opus_data);

    match decode_result {
        Ok(pcm_data) => {
            // info!("decode success.");
            let is_stereo = channels == 2;

            if !is_stereo {
                info!("is_stereo is false. 不是立体声");
                //因为 p3文件是单声道的，而我们的 I2S 配置是双声道的，所以需要将单声道数据转换成双声道数据。
                let pcm_mono_data_len = pcm_data.len();
                // 1. 清空旧数据，但保留容量（不释放内存）
                pcm_buffer.clear();
                pcm_buffer.resize(pcm_mono_data_len * 2, 0);
                // let mut pcm_stereo_buffer = vec![0i16; pcm_mono_data_len * 2];

                info!("遍历单声道样本，并复制到立体声缓冲区的左右声道");
                // 2. 遍历单声道样本，并复制到立体声缓冲区的左右声道
                for i in 0..pcm_mono_data_len {
                    let sample = pcm_data[i];
                    pcm_buffer[i * 2] = sample; // 左声道
                    pcm_buffer[i * 2 + 1] = sample; // 右声道
                }

                // info!("把立体声缓冲区转换为u8字节数组");
                let pcm_stereo_bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        pcm_buffer.as_ptr() as *const u8,
                        pcm_buffer.len() * std::mem::size_of::<i16>(),
                    )
                };

                // info!("把u8字节数组写入音频播放器");
                codec.lock().unwrap().output_data(pcm_stereo_bytes).unwrap();
                // play_pcm_audio(i2s_driver, pcm_stereo_bytes);
            } else {
                // info!("is_stereo is true. 立体声,直接转为u8字节数组");
                let pcm_stereo_bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        pcm_data.as_ptr() as *const u8,
                        pcm_data.len() * std::mem::size_of::<i16>(),
                    )
                };

                // info!("把u8字节数组写入音频播放器");
                codec.lock().unwrap().output_data(pcm_stereo_bytes).unwrap();
            }
        }
        Err(e) => {
            info!("Opus decode error: {:?}", e);
            return;
        }
    }
}

fn decode_opus_audio(
    // codec: Arc<Mutex<dyn AudioCodec + 'static>>,
    opus_decoder: &mut OpusAudioDecoder,
    // mut i2s_driver: MutexGuard<'_, I2sDriver<'_, I2sBiDir>>,
    opus_data: Vec<u8>,
    pcm_buffer: &mut Vec<i16>,
) -> anyhow::Result<Vec<u8>> {
    // let sample_rate = 16000; //# 采样率固定为16000Hz
    let channels = 2; //# 双声道
                      // let channels = 1; //# 双声道

    let decode_result = opus_decoder.decode(&opus_data);

    // let mut decoder = Box::new(OpusAudioDecoder::new(sample_rate, channels).unwrap());
    // let decode_result = decoder.decode(&opus_data);

    match decode_result {
        Ok(pcm_data) => {
            // info!("decode success.");
            let is_stereo = channels == 2;

            if !is_stereo {
                info!("is_stereo is false. 不是立体声");
                //因为 p3文件是单声道的，而我们的 I2S 配置是双声道的，所以需要将单声道数据转换成双声道数据。
                let pcm_mono_data_len = pcm_data.len();
                // 1. 清空旧数据，但保留容量（不释放内存）
                pcm_buffer.clear();
                pcm_buffer.resize(pcm_mono_data_len * 2, 0);
                // let mut pcm_stereo_buffer = vec![0i16; pcm_mono_data_len * 2];

                info!("遍历单声道样本，并复制到立体声缓冲区的左右声道");
                // 2. 遍历单声道样本，并复制到立体声缓冲区的左右声道
                for i in 0..pcm_mono_data_len {
                    let sample = pcm_data[i];
                    pcm_buffer[i * 2] = sample; // 左声道
                    pcm_buffer[i * 2 + 1] = sample; // 右声道
                }

                info!("把立体声缓冲区转换为u8字节数组");
                let pcm_stereo_bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        pcm_buffer.as_ptr() as *const u8,
                        pcm_buffer.len() * std::mem::size_of::<i16>(),
                    )
                };

                Ok(pcm_stereo_bytes.to_vec())
                // info!("把u8字节数组写入音频播放器");
                // codec.lock().unwrap().output_data(pcm_stereo_bytes).unwrap();
                // play_pcm_audio(i2s_driver, pcm_stereo_bytes);
            } else {
                // info!("is_stereo is true. 立体声,直接转为u8字节数组");
                let pcm_stereo_bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        pcm_data.as_ptr() as *const u8,
                        pcm_data.len() * std::mem::size_of::<i16>(),
                    )
                };
                Ok(pcm_stereo_bytes.to_vec())
                // info!("把u8字节数组写入音频播放器");
                // codec.lock().unwrap().output_data(pcm_stereo_bytes).unwrap();
            }
        }
        Err(e) => {
            info!("Opus decode error: {:?}", e);
            return Err(e);
        }
    }
}

fn play_pcm_audio(mut i2s_driver: MutexGuard<'_, I2sDriver<'_, I2sBiDir>>, audio_data: &[u8]) {
    const CHUNK_SIZE: usize = 4096;
    for chunk in audio_data.chunks(CHUNK_SIZE) {
        // 4. 逐块写入I2S驱动
        match i2s_driver.write(chunk, BLOCK) {
            Ok(bytes_written) => {
                // 打印一些进度信息，方便调试
                // info!("Successfully wrote {} bytes to I2S.", bytes_written);
            }
            Err(e) => {
                // 如果在写入过程中出错，打印错误并跳出循环
                info!("I2S write error on a chunk: {:?}", e);
                break;
            }
        }
    }
}

fn play_opus_audio(
    mut opus_decoder: Arc<Mutex<Box<OpusAudioDecoder>>>,
    mut i2s_driver: MutexGuard<'_, I2sDriver<'_, I2sBiDir>>,
    opus_data: Vec<u8>,
    pcm_buffer: &mut Vec<i16>,
) {
    let sample_rate = 16000; //# 采样率固定为16000Hz
    let channels = 2; //# 单声道

    // decoder = decoder.decode(sample_rate, channels);

    let decode_result = opus_decoder.lock().unwrap().decode(&opus_data);

    // let mut decoder = Box::new(OpusAudioDecoder::new(sample_rate, channels).unwrap());
    // let decode_result = decoder.decode(&opus_data);

    match decode_result {
        Ok(pcm_data) => {
            // info!("decode success.");
            let is_stereo = channels == 2;

            if !is_stereo {
                //因为 p3文件是单声道的，而我们的 I2S 配置是双声道的，所以需要将单声道数据转换成双声道数据。
                let pcm_mono_data_len = pcm_data.len();
                // 1. 清空旧数据，但保留容量（不释放内存）
                pcm_buffer.clear();
                pcm_buffer.resize(pcm_mono_data_len * 2, 0);
                // let mut pcm_stereo_buffer = vec![0i16; pcm_mono_data_len * 2];

                // 2. 遍历单声道样本，并复制到立体声缓冲区的左右声道
                for i in 0..pcm_mono_data_len {
                    let sample = pcm_data[i];
                    pcm_buffer[i * 2] = sample; // 左声道
                    pcm_buffer[i * 2 + 1] = sample; // 右声道
                }

                let pcm_stereo_bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        pcm_buffer.as_ptr() as *const u8,
                        pcm_buffer.len() * std::mem::size_of::<i16>(),
                    )
                };
                play_pcm_audio(i2s_driver, pcm_stereo_bytes);
            } else {
                let pcm_stereo_bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        pcm_data.as_ptr() as *const u8,
                        pcm_data.len() * std::mem::size_of::<i16>(),
                    )
                };
                play_pcm_audio(i2s_driver, pcm_stereo_bytes);
            }
        }
        Err(e) => {
            info!("Opus decode error: {:?}", e);
            return;
        }
    }
}

fn run_audio_decode_task(
    xz_event_rx: Receiver<AppEvent>,
    pcm_sender: SyncSender<Vec<u8>>,
    // codec: Arc<Mutex<dyn AudioCodec + 'static>>,
) {
    let sample_rate = AUDIO_INPUT_SAMPLE_RATE as i32; //# 采样率固定为16000Hz
    let channels = 2; //# 单声道

    let task_closure: Box<dyn FnOnce() + Send> = Box::new(move || {
        info!("Starting audio decode task!");
        let mut opus_decoder = OpusAudioDecoder::new(
            sample_rate,
            channels,
            OPUS_FRAME_DURATION_MS.try_into().unwrap(),
        )
        .unwrap();
        let mut shared_pcm_buffer: Vec<i16> = Vec::with_capacity(4096);

        let mut pcm_buffer: Vec<u8> = Vec::with_capacity(38400);
        let mut cached_packet_count = 0;
        // let mut tts_start = true;
        loop {
            match xz_event_rx.recv() {
                Ok(event) => match event {
                    AppEvent::AudioPacketReceived(audio_packet) => {
                        match decode_opus_audio(
                            // codec.clone(),
                            &mut opus_decoder,
                            audio_packet.payload,
                            &mut shared_pcm_buffer,
                        ) {
                            Ok(mut pcm_data) => {
                                pcm_buffer.append(&mut pcm_data);
                                if cached_packet_count < 10 {
                                    cached_packet_count += 1;
                                    continue;
                                } else {
                                    cached_packet_count = 0;
                                }
                                let cached_pcm = pcm_buffer.clone();
                                match pcm_sender.send(cached_pcm) {
                                    Ok(_) => {
                                        pcm_buffer.clear();
                                        // info!("Send pcm data success.");
                                    }
                                    Err(e) => {
                                        error!("Send decoded opus data(pcm data) error: {:?}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Failed to decode audio: {}", e);
                            }
                        }
                    }
                    AppEvent::TTSStop => {
                        //把缓存里剩下的的PCM数据发送出去
                        let cached_pcm = pcm_buffer.clone();
                        match pcm_sender.send(cached_pcm) {
                            Ok(_) => {
                                cached_packet_count = 0;
                                // tts_start = true;
                                pcm_buffer.clear();
                                // info!("Send pcm data success.");
                            }
                            Err(e) => {
                                error!("Send decoded opus data(pcm data) error: {:?}", e);
                            }
                        }
                    }
                    _ => {
                        info!("Received unhandled event: {:?}", event);
                    }
                },
                Err(_) => {
                    info!("Event channel closed, exiting event loop");
                }
            }
        }
    });

    info!("只装箱一次！");

    let closure_box = Box::new(task_closure);
    let closure_ptr = Box::into_raw(closure_box);

    info!("try to call xTaskCreatePinnedToCore in the unsafe block");
    unsafe {
        let res = esp_idf_sys::xTaskCreatePinnedToCore(
            Some(c_task_trampoline),
            b"decode_task\0".as_ptr() as *const u8,
            16 * 1024,
            closure_ptr as *mut c_void,
            5,
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

fn play_p3_audio(mut i2s_driver: MutexGuard<'_, I2sDriver<'_, I2sBiDir>>) {
    const P3_DATA: &'static [u8] = include_bytes!("../assets/activation.p3");

    info!(
        "Embedded p3 data size: {} bytes. Starting playback...",
        P3_DATA.len()
    );

    const CHUNK_SIZE: usize = 4096;

    info!("Starting playback in chunks of {} bytes...", CHUNK_SIZE);

    if P3_DATA.len() < 4 {
        error!("P3 data is too small to be valid.");
        return;
    }

    let p3_data_len = P3_DATA.len();
    info!("P3 data length: {} bytes", p3_data_len);

    let sample_rate = AUDIO_INPUT_SAMPLE_RATE as i32; //# 采样率固定为16000Hz
    let channels = 1; //# 单声道
    let mut opus_decoder = OpusAudioDecoder::new(
        sample_rate,
        channels,
        OPUS_FRAME_DURATION_MS.try_into().unwrap(),
    )
    .unwrap();

    let mut offset = 0;

    while offset < p3_data_len {
        let len: [u8; 2] = P3_DATA[offset + 2..offset + 4].try_into().unwrap();
        let frame_len = u16::from_be_bytes(len) as usize;

        let opus_data = &P3_DATA[(offset + 4)..(offset + 4 + frame_len)];
        offset += 4 + frame_len;
        info!("offset {} bytes...", offset);

        // decoder = decoder.decode(sample_rate, channels);
        let decode_result = opus_decoder.decode(opus_data);

        match decode_result {
            Ok(pcm_data) => {
                //因为 p3文件是单声道的，而我们的 I2S 配置是双声道的，所以需要将单声道数据转换成双声道数据。
                let pcm_mono_data_len = pcm_data.len();

                let mut pcm_stereo_buffer = vec![0i16; pcm_mono_data_len * 2];

                // 2. 遍历单声道样本，并复制到立体声缓冲区的左右声道
                for i in 0..pcm_mono_data_len {
                    let sample = pcm_data[i];
                    pcm_stereo_buffer[i * 2] = sample; // 左声道
                    pcm_stereo_buffer[i * 2 + 1] = sample; // 右声道
                }

                let pcm_stereo_bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        pcm_stereo_buffer.as_ptr() as *const u8,
                        pcm_stereo_buffer.len() * std::mem::size_of::<i16>(),
                    )
                };

                // 如果p3是双声道的，或者使用了单声道的 I2S 配置，我们就可以直接使用 decode 后的音频数据。
                // // 1. 首先，获取一个指向有效数据的切片
                // let pcm_slice: &[i16] = &pcm_data;

                // // 2. 使用unsafe块来进行零成本的类型转换
                // let pcm_bytes: &[u8] = unsafe {
                //     // a. 获取i16切片的裸指针和长度（以i16为单位）
                //     let ptr = pcm_slice.as_ptr();
                //     let len_in_i16 = pcm_slice.len();

                //     // b. 使用`core::slice::from_raw_parts`来创建一个新的字节切片
                //     //    - 将i16指针强制转换成u8指针
                //     //    - 将长度（以i16为单位）乘以每个i16的字节数（2），得到总的字节长度
                //     core::slice::from_raw_parts(
                //         ptr as *const u8,
                //         len_in_i16 * std::mem::size_of::<i16>(),
                //     )
                // };

                // // 3. 使用 .chunks() 方法将整个PCM数据切分成多个小块
                for chunk in pcm_stereo_bytes.chunks(CHUNK_SIZE) {
                    // 4. 逐块写入I2S驱动
                    //    i2s_driver.write() 会阻塞，直到这一小块数据被成功写入DMA
                    match i2s_driver.write(chunk, BLOCK) {
                        Ok(bytes_written) => {
                            // 打印一些进度信息，方便调试
                            info!("Successfully wrote {} bytes to I2S.", bytes_written);
                        }
                        Err(e) => {
                            // 如果在写入过程中出错，打印错误并跳出循环
                            info!("I2S write error on a chunk: {:?}", e);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                info!("Opus decode error: {:?}", e);
                return;
            }
        }
    }
}
