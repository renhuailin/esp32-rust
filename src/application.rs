use std::{
    collections::VecDeque,
    ffi::{c_void, CStr},
    ptr,
    sync::{
        mpsc::{self, channel, Receiver, Sender, SyncSender},
        Arc, Mutex, MutexGuard,
    },
    thread,
    time::Duration,
};

use anyhow::{Error, Result};
use esp_idf_hal::{
    delay::BLOCK,
    i2s::{I2sBiDir, I2sDriver},
    task::thread::ThreadSpawnConfiguration,
};

use esp_idf_sys::{
    esp_partition_find, esp_partition_get, esp_partition_next,
    esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_OTA_0,
    esp_partition_type_t_ESP_PARTITION_TYPE_APP,
};
use log::{error, info, warn};

use crate::{
    audio::{
        codec::{
            audio_codec::AudioCodec,
            opus::{decoder::OpusAudioDecoder, encoder::OpusAudioEncoder},
            AudioStreamPacket, MAX_AUDIO_PACKETS_IN_QUEUE, OPUS_FRAME_DURATION_MS,
        },
        processor::{
            afe_audio_processor::AfeAudioProcessor, audio_processor::AudioProcessor,
            no_audio_processor::NoAudioProcessor,
        },
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
                                                                 // 但为了简单起见，我们先只用Mutex
}

impl SharedAudioState {
    pub fn new() -> Self {
        Self {
            buffer: Mutex::new(VecDeque::new()),
            audio_packet_buffer: Mutex::new(VecDeque::new()),
        }
    }
}

const VERSION: &str = env!("CARGO_PKG_VERSION");
fn check_new_version() -> anyhow::Result<()> {
    info!("check_new_version, current version is {}", VERSION);

    let mut client = HttpClient::wrap(EspHttpConnection::new(&Default::default())?);
    check_for_updates(&mut client)?;
    Ok(())
}

mod http_status {
    pub const OK: u16 = 200;
    pub const NOT_MODIFIED: u16 = 304;
}

pub fn check_for_updates(client: &mut HttpClient<EspHttpConnection>) -> anyhow::Result<()> {
    let mut ota = EspOta::new()?;

    let current_version = VERSION;
    info!("Current version: {current_version}");

    info!("Checking for updates...");

    let headers = [
        ("Accept", "application/octet-stream"),
        ("X-Esp32-Version", &current_version),
    ];

    let ota_firmware_url = "http://192.168.1.145:3000/api/v1/ota/update";

    let request = client.request(Method::Get, ota_firmware_url, &headers)?;
    let response = request.submit()?;

    if response.status() == http_status::NOT_MODIFIED {
        info!("OTA: Already up to date");
    } else if response.status() == http_status::OK {
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

pub struct Application {
    state: DeviceState,
    protocol: WebSocketProtocol,
    board: Box<dyn Board<WifiDriver = Esp32WifiDriver>>,

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
    audio_decode_queue: Arc<Mutex<VecDeque<AudioStreamPacket>>>, //待解码的音频队列
    busy_decoding_audio: Arc<Mutex<bool>>, //正在解码音频,TODO:: 在c++代码中，如果正在解码音频，则不播放音频

    decode_task_sender: Sender<AppEvent>,
    decode_task_receiver: Option<Receiver<AppEvent>>,
    audio_test_mode: bool, //音频测试模式,在这个模式下，并不真的发送音频数据到服务器端，
    // 而是直接保存在这个字段的buffer里，然后在音箱端解码，播放出来，
    // 主要是用于测试音频采集是否正常及解码是否正常。
    shared_audio_state: Arc<SharedAudioState>,
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
        board.on_touch_button_clicked(Box::new(move || {
            // println!("Touch button clicked");
            if let Err(e) = sender.send(AppEvent::BootButtonClicked) {
                log::error!("Failed to send BootButtonClicked event: {:?}", e);
            }
        }));

        let sender1 = inner_sender.clone();
        board.on_volume_button_clicked(Box::new(move || {
            // println!("Volume button clicked");
            if let Err(e) = sender1.send(AppEvent::VolumeButtonClicked) {
                log::error!("Failed to send VolumeButtonClicked event: {:?}", e);
            }
        }));

        board.init()?;
        info!("board init success");

        // info!("check new version ...");
        // check_new_version()?;

        let mac_address = board.get_wifi_driver().get_mac_address()?;
        let sender_for_protocol = inner_sender.clone();
        let protocol = WebSocketProtocol::new(mac_address.as_str(), sender_for_protocol);

        //待发送的音频队列
        let audio_packet_queue = Arc::new(Mutex::new(
            VecDeque::<AudioStreamPacket>::with_capacity(MAX_AUDIO_PACKETS_IN_QUEUE),
        ));
        let audio_decode_queue = Arc::new(Mutex::new(
            VecDeque::<AudioStreamPacket>::with_capacity(MAX_AUDIO_PACKETS_IN_QUEUE),
        ));

        //先使用NoAudioProcessor，等以后有时间再改成AfeAudioProcessor，因为我测试很久，AfeAudioProcessor的总是报堆栈溢出。
        let audio_processor = Arc::new(Mutex::new(
            AfeAudioProcessor::new(board.get_audio_codec().clone()).unwrap(),
        ));

        // let audio_processor = Arc::new(Mutex::new(NoAudioProcessor::new(16000)));

        let shared_audio_state = Arc::new(SharedAudioState::new());

        let sample_rate = 16000; //# 采样率固定为16000Hz
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
            audio_decode_queue,
            busy_decoding_audio: Arc::new(Mutex::new(false)),
            audio_test_mode: false,
            shared_audio_state,
            opus_decoder,
            opus_encoder,
            inner_pcm_tx: pcm_tx,
            inner_pcm_rx: Some(pcm_rx),
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

        let (pcm_tx, pcm_rx) = mpsc::channel::<Vec<i16>>();

        let inner_sender = self.inner_sender.clone();

        let audio_test_mode = self.audio_test_mode.clone();
        let audio_state = Arc::clone(&self.shared_audio_state);

        let opus_encoder_arc = Arc::clone(&self.opus_encoder);
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
                                sample_rate: 16000,
                                frame_duration: 60,
                                timestamp: 0,
                                payload: opus_data,
                            };

                            let sender = inner_sender1.clone();

                            if audio_test_mode {
                                // sender.send(XzEvent::AudioPacketReceived(packet)).unwrap();
                                audio_state1
                                    .audio_packet_buffer
                                    .lock()
                                    .unwrap()
                                    .push_back(packet);
                            } else {
                                if let Err(e) = sender.send(AppEvent::AddAudioPacketToQueue(packet))
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
            });
        match encode_thread {
            Ok(_) => {}
            Err(_) => {
                error!("Failed to create encode thread");
            }
        }

        let audio_processor = Arc::clone(&self.audio_processor);

        let codec_clone_for_pcm_player = Arc::clone(&codec_arc);

        let audio_state = Arc::clone(&self.shared_audio_state);

        audio_processor
            .lock()
            .unwrap()
            .on_output(Box::new(move |data| {
                // 发送到编码线程,编码成opus.
                if let Err(e) = pcm_tx.send(data) {
                    // 如果发送失败（比如编码线程挂了），打印个日志，不要 panic
                    error!("Failed to send PCM to encoder: {:?}", e);
                }
            }));

        // // 启动一个线程来读取音频数据
        // ThreadSpawnConfiguration {
        //     name: Some(b"audio_loop\0"),
        //     stack_size: 4096 * 2,
        //     priority: 5,
        //     pin_to_core: Some(1.into()), // 绑定到 Core 1

        //     // 关键点：虽然这里没有直接的 "stack_in_psram" 字段，
        //     // 但我们可以通过设置 inherit 为 false 来避免继承父线程的配置
        //     inherit: false,
        //     ..Default::default()
        // }
        // .set()
        // .unwrap();
        // let _ = thread::spawn(move || {
        //     audio_loop(codec_clone, audio_processor);
        // });
        // ThreadSpawnConfiguration::default().set().unwrap();

        let task_closure: Box<dyn FnOnce() + Send> = Box::new(move || {
            audio_loop(codec_clone, audio_processor);
        });

        let closure_box = Box::new(task_closure);
        let closure_ptr = Box::into_raw(closure_box);

        info!("try to call xTaskCreatePinnedToCore in the unsafe block");
        unsafe {
            let res = esp_idf_sys::xTaskCreatePinnedToCore(
                Some(c_task_trampoline),
                b"audio_loop\0".as_ptr() as *const u8,
                4096 * 2,
                closure_ptr as *mut c_void,
                8,
                ptr::null_mut(),
                1,
            );
            // if res != esp_idf_sys::pdPass {
            //     // 如果创建失败，记得收回内存，否则会泄漏
            //     let _ = Box::from_raw(closure_ptr);
            //     error!("Failed to create task");
            // }
        }

        info!("启动解码线程 start_output_audio ...");
        self.start_output_audio();

        self.set_device_state(DeviceState::Idle);

        let codec_for_opus_player = Arc::clone(&codec_arc);

        let pcm_player_codec = Arc::clone(&codec_for_opus_player);

        //启动音频输出线程
        info!("启动音频输出线程 start pcm_player_thread ...");
        if let Some(pcm_rx) = self.inner_pcm_rx.take() {
            ThreadSpawnConfiguration {
                name: Some(b"pcm_player_thread\0"),
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

        // self.audio_alert("welcome");

        // 处理内部事件
        self.event_loop()?;
        Ok(())
    }

    ///播放音频提醒
    pub fn audio_alert(&mut self, message: &str) {
        self.play_p3_audio(message);
    }

    fn event_loop(&mut self) -> Result<(), Error> {
        let mut shared_pcm_buffer: Vec<i16> = Vec::with_capacity(4096);
        let inner_sender = self.inner_sender.clone();
        let audio_state = Arc::clone(&self.shared_audio_state);
        let audio_test_mode = self.audio_test_mode;
        let audio_packet_send_queue_arc = Arc::clone(&self.audio_packet_queue);
        let codec_for_opus_player = Arc::clone(&self.board.get_audio_codec());

        loop {
            let opus_decoder = Arc::clone(&self.opus_decoder);
            match self.inner_receiver.recv() {
                Ok(event) => {
                    match event {
                        AppEvent::BootButtonClicked => {
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
                            // if self.state == DeviceState::Idle {
                            //     self.set_device_state(DeviceState::Listening);
                            // } else if self.state == DeviceState::Listening {
                            //     self.set_device_state(DeviceState::Idle);
                            // }
                            self.toggle_device_state();

                            if audio_test_mode {
                                //下面的代码是用于PCM音频本地回放测试用的
                                // let mut pcm_data = {
                                //     let mut buffer = audio_state.buffer.lock().unwrap();
                                //     std::mem::take(&mut *buffer)
                                // };
                                // let pcm_u8: &[u8] = pcm_data.make_contiguous();

                                // info!("准备播放音频数据，内容: {} ", pcm_u8.len());

                                // codec_clone_for_pcm_player
                                //     .lock()
                                //     .unwrap()
                                //     .test_play_pcm(pcm_u8)
                                //     .unwrap();

                                // 下面的代码是用于Opus音频本地回放测试用的
                                let opus_data: VecDeque<AudioStreamPacket> = {
                                    let mut buffer =
                                        audio_state.audio_packet_buffer.lock().unwrap();
                                    std::mem::take(&mut *buffer)
                                };

                                for packet in opus_data {
                                    inner_sender
                                        .send(AppEvent::AudioPacketReceived(packet))
                                        .unwrap();
                                }
                            }
                        }
                        // XzEvent::WebSocketConnected => {
                        //     info!("Connected,try to send hello message");
                        //     // // send client hello message
                        //     // if let Some(client) = &mut self.client {
                        //     //     if client.is_connected() {
                        //     //         let hello_message = ClientHelloMessage::new().unwrap();
                        //     //         info!("Worker thread: Sending hello message...");
                        //     //         match client.send(FrameType::Text(false), hello_message.as_bytes()) {
                        //     //             Ok(_) => info!("Worker thread: Hello message sent!"),
                        //     //             Err(e) => info!("Worker thread: Send error: {:?}", e),
                        //     //         }
                        //     //     } else {
                        //     //         info!("Worker thread: Client not connected, cannot send.");
                        //     //     }
                        //     // }
                        // }
                        AppEvent::WebSocketClosed => {
                            info!("WebSocketClosed");
                            // board.SetPowerSaveMode(true);
                            // Schedule([this]() {
                            //     auto display = Board::GetInstance().GetDisplay();
                            //     display->SetChatMessage("system", "");
                            //     SetDeviceState(kDeviceStateIdle);
                            // }); });
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
                                        if message_type == "tts" {
                                            if let Some(state) = message["state"].as_str() {
                                                if state == "start" {
                                                    // TODO:: 研究一下 aborted 是干什么的
                                                    // self.aborted = false;
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

                                                    self.decode_task_sender
                                                        .send(AppEvent::TTSStop)
                                                        .unwrap();

                                                    // TODO:: 看一下 background_task_ 在我们这里怎么实现，他的作用应该是等后台任务完成。
                                                    // background_task_->WaitForCompletion();
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
                                //     "XzEvent::AudioPacketReceived - 从服务器端接收到的音频数据包"
                                // );
                                if self.state == DeviceState::Speaking
                                    && audio_packet_send_queue_arc.lock().unwrap().len()
                                        < MAX_AUDIO_PACKETS_IN_QUEUE
                                {
                                    // 在子线程中处理音频解码
                                    // let mut audio_decode_queue =
                                    //     self.audio_decode_queue.lock().unwrap();
                                    // audio_decode_queue.push_back(audio_stream_packet);

                                    match self
                                        .decode_task_sender
                                        .clone()
                                        .send(AppEvent::AudioPacketReceived(audio_stream_packet))
                                    {
                                        Ok(_) => {
                                            // info!("send audio decode event ok");
                                        }
                                        Err(e) => {
                                            error!("send audio decode event error: {:?}", e);
                                        }
                                    }

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
                            self.set_device_state(DeviceState::Idle);
                            error!("ProtocolNetworkError: {:?}", err);
                        }

                        AppEvent::PlayAudioAlert(message) => {
                            self.audio_alert(&message);
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
                // display->SetEmotion("neutral");
                // audio_processor_->Stop();
                // wake_word_->StartDetection();
                self.audio_processor.lock().unwrap().stop();
            }
            DeviceState::Activating => {
                info!(
                    "Device state changed from {:?} to {:?}",
                    previous_state, self.state
                );
            }
            DeviceState::WifiConfiguring => {
                info!(
                    "Device state changed from {:?} to {:?}",
                    previous_state, self.state
                );
            }
            DeviceState::Connecting => {
                info!(
                    "Device state changed from {:?} to {:?}",
                    previous_state, self.state
                );
            }
            // DeviceState::DeviceStateAudioTesting => todo!(),
            DeviceState::Speaking => {
                info!(
                    "Device state changed from {:?} to {:?}",
                    previous_state, self.state
                );

                if self.listening_mode != ListeningMode::Realtime {
                    self.audio_processor.lock().unwrap().stop();

                    //                     #if CONFIG_USE_AFE_WAKE_WORD
                    //             wake_word_->StartDetection();
                    // #else
                    //             wake_word_->StopDetection();
                    // #endif
                }
                self.reset_decoder();
            }
            DeviceState::Listening => {
                info!(
                    "Listening state changed from {:?} to {:?}",
                    previous_state, self.state
                );
                if !self.audio_processor.lock().unwrap().is_running() {
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
                    self.audio_processor.lock().unwrap().start();
                }
            }
            DeviceState::Starting => {
                info!(
                    "Starting state changed from {:?} to {:?}",
                    previous_state, self.state
                )
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
                if !self.protocol.is_audio_channel_opened() {
                    self.set_device_state(DeviceState::Connecting);
                    if !self.protocol.open_audio_channel().unwrap_or(false) {
                        return;
                    }
                }

                self.set_listening_mode(if self.aec_mode == AecMode::Off {
                    ListeningMode::AutoStop
                } else {
                    ListeningMode::Realtime
                });
            }
            DeviceState::Speaking => {
                if let Err(e) = self.protocol.send_abort_speaking(AbortReason::None) {
                    error!("Failed to send abort speaking: {:?}", e);
                }
            }
            DeviceState::Listening => {
                info!("DeviceState::Listening - Closing audio channel...");
                // if let Err(e) = self.protocol.close_audio_channel() {
                //     error!("Failed to close_audio_channel: {:?}", e);
                // }

                {
                    let mut audio_processor = self.audio_processor.lock().unwrap();
                    audio_processor.stop();
                }

                self.stop_listening();
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
        self.audio_decode_queue.lock().unwrap().clear();
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

        info!("Starting playback in chunks of {} bytes...", CHUNK_SIZE);

        if p3_data.len() < 4 {
            error!("P3 data is too small to be valid.");
            return;
        }

        let p3_data_len = p3_data.len();
        info!("P3 data length: {} bytes", p3_data_len);

        let sample_rate = 16000; //# 采样率固定为16000Hz
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
}

fn audio_loop(
    audio_codec: Arc<Mutex<dyn AudioCodec>>,
    audio_processor: Arc<Mutex<dyn AudioProcessor>>,
) {
    // let mut codec = audio_codec.lock().unwrap();
    // codec.set_output_volume(50);
    // let codec_arc = Arc::clone(&audio_codec);
    // let codec_arc1 = Arc::clone(&audio_codec);
    let audio_processor_arc = Arc::clone(&audio_processor);

    let feed_size = audio_processor.lock().unwrap().get_feed_size();
    info!("application: feed_size: {}", feed_size);
    // const READ_CHUNK_SIZE: usize = 1024;
    let mut read_buffer = vec![0u8; feed_size];
    // let mut read_buffer = vec![0u8; 1024];
    loop {
        start_audio_input(
            Arc::clone(&audio_codec),
            audio_processor_arc.clone(),
            &mut read_buffer,
        );

        let codec_arc = Arc::clone(&audio_codec);
        if codec_arc.lock().unwrap().output_enabled() {
            start_audio_output(codec_arc, audio_processor_arc.clone());
        }

        // thread::sleep(Duration::from_millis(10));
    }
}

fn start_audio_output(
    codec_arc: Arc<Mutex<dyn AudioCodec + 'static>>,
    audio_processor: Arc<Mutex<dyn AudioProcessor + 'static>>,
) {
    info!("application: start_audio_output");
}
fn start_audio_input(
    codec: Arc<Mutex<dyn AudioCodec + 'static>>,
    audio_processor: Arc<Mutex<dyn AudioProcessor + 'static>>,
    mut read_buffer: &mut Vec<u8>,
) {
    thread::sleep(Duration::from_millis((OPUS_FRAME_DURATION_MS / 2) as u64));
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

    // vTaskDelay(pdMS_TO_TICKS(OPUS_FRAME_DURATION_MS / 2));

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

    if is_running && feed_size > 0 {
        // let start = Instant::now();
        // 2. 读取音频 (耗时操作，不要持有 processor 的锁)
        // read_buffer 需要扩容以容纳数据
        // if read_buffer.len() < feed_size * 2 {
        //     // 假设是 i16，需要 2 倍字节
        //     read_buffer.resize(feed_size * 2, 0);
        // }
        // read_buffer.resize(1024, 0);

        let bytes_read = codec
            .lock()
            .unwrap()
            .read_audio_data(&mut read_buffer) // 确保 read_audio_data 不会溢出
            .unwrap();

        info!("从codec读取音频数据的bytes_read = {}", bytes_read);

        // let duration = start.elapsed();
        // info!("从codec读取音频数据 耗时: {:?}", duration);

        // info!(
        //     "application: read data from codec es7210, bytes_read: {}",
        //     bytes_read
        // );

        if bytes_read > 0 {
            let audio_data = &read_buffer[..bytes_read];

            // info!("准备播放音频数据，内容: {:?} ", audio_data);

            // let audio_data_md5 = calc_md5_builtin(audio_data);
            // info!(
            //     "音频数据： Feed 前 -  md5: {} - 内容: {:?} ",
            //     audio_data_md5, audio_data
            // );
            // let start = Instant::now();
            let bytes_to_i16_result = bytes_to_i16_slice(&audio_data).unwrap();

            // let duration = start.elapsed();
            // info!("数据转换 耗时: {:?}", duration);
            info!("application: feed data to audio processor");

            info!("真正喂进去的 i16 长度: {}", bytes_to_i16_result.len());

            // // 3. 再次获取锁进行 feed
            // // 此时 codec 的锁已经释放了，避免交叉死锁
            // let start = Instant::now();
            audio_processor.lock().unwrap().feed(&bytes_to_i16_result);
            // let duration = start.elapsed();
            // info!("Feed 数据 耗时: {:?}", duration);
        }
    }

    // thread::sleep(Duration::from_millis(10));
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
    let sample_rate = 16000; //# 采样率固定为16000Hz
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

    let sample_rate = 16000; //# 采样率固定为16000Hz
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
