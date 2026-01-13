use std::{
    collections::VecDeque,
    sync::{
        mpsc::{channel, Receiver, Sender},
        Arc, Mutex, MutexGuard,
    },
    thread,
    time::Duration,
};

use anyhow::{Error, Result};
use esp_idf_hal::{
    i2c::I2cDriver,
    i2s::{I2sBiDir, I2sDriver},
    peripheral,
};
use esp_idf_svc::{eventloop::EspSystemEventLoop, wifi::WifiDeviceId};
use esp_idf_sys::es32_component_opus::OPUS_GET_INBAND_FEC_REQUEST;
use log::{error, info, warn};

use crate::{
    audio::{
        codec::{
            self,
            audio_codec::AudioCodec,
            opus::{decoder::OpusAudioDecoder, encoder::OpusAudioEncoder},
            AudioStreamPacket, MAX_AUDIO_PACKETS_IN_QUEUE, OPUS_FRAME_DURATION_MS,
        },
        processor::{audio_processor::AudioProcessor, no_audio_processor::NoAudioProcessor},
    },
    axp173::{Axp173, Ldo},
    boards::{board::Board, jianglian_s3cam_board},
    common::{
        converter::bytes_to_i16_slice,
        enums::{AbortReason, AecMode, DeviceState, ListeningMode},
        event::XzEvent,
    },
    protocols::{protocol::Protocol, websocket::ws_protocol::WebSocketProtocol},
    wifi::{self, Esp32WifiDriver, WifiStation},
};
use shared_bus::{BusManager, BusManagerSimple};

pub struct ApplicationConfig {
    sender: Sender<XzEvent>,
}

pub struct Application {
    state: DeviceState,
    protocol: WebSocketProtocol,
    board: Box<dyn Board<WifiDriver = Esp32WifiDriver>>,
    inner_sender: Sender<XzEvent>,
    inner_receiver: Receiver<XzEvent>,
    aec_mode: AecMode,
    listening_mode: ListeningMode,
    audio_processor: Arc<Mutex<dyn AudioProcessor>>,
    audio_packet_queue: Arc<Mutex<VecDeque<AudioStreamPacket>>>, //待发送的音频队列
    audio_decode_queue: Arc<Mutex<VecDeque<AudioStreamPacket>>>, //待解码的音频队列
    busy_decoding_audio: Arc<Mutex<bool>>,                       //正在解码音频

    decode_task_sender: Sender<XzEvent>,
    decode_task_receiver: Option<Receiver<XzEvent>>,
}

impl Application {
    pub fn new() -> Result<Self> {
        let (inner_sender, inner_receiver): (Sender<XzEvent>, Receiver<XzEvent>) = channel();

        let (decode_task_sender, decode_task_receiver): (Sender<XzEvent>, Receiver<XzEvent>) =
            channel();

        let mut board = Box::new(jianglian_s3cam_board::JiangLianS3CamBoard::new()?);

        let sender = inner_sender.clone();
        board.on_touch_button_clicked(Box::new(move || {
            // println!("Touch button clicked");
            if let Err(e) = sender.send(XzEvent::BootButtonClicked) {
                log::error!("Failed to send BootButtonClicked event: {:?}", e);
            }
        }));

        let sender1 = inner_sender.clone();
        board.on_volume_button_clicked(Box::new(move || {
            // println!("Volume button clicked");
            if let Err(e) = sender1.send(XzEvent::VolumeButtonClicked) {
                log::error!("Failed to send VolumeButtonClicked event: {:?}", e);
            }
        }));

        board.init()?;
        info!("board init success");

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
            audio_processor: Arc::new(Mutex::new(NoAudioProcessor::new(16000))),
            audio_packet_queue,
            audio_decode_queue,
            busy_decoding_audio: Arc::new(Mutex::new(false)),
        };

        Ok(instance)
    }

    pub fn start(&mut self) -> Result<(), Error> {
        self.set_device_state(DeviceState::Starting);
        let codec_arc = self.board.get_audio_codec();
        // let codec_arc = Arc::new(Mutex::new(codec));

        // let codec = self.board.get_audio_codec();
        codec_arc.lock().unwrap().start();

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
        //     if let Err(e) = sender1.send(XzEvent::AudioPacketReceived(packet.clone())) {
        //         log::error!("Failed to send WebsocketTextMessageReceived event: {:?}", e);
        //     }
        //     Ok(())
        // })?;

        // 启动一个线程来读取音频数据
        const THREAD_STACK_SIZE: usize = 96 * 1024;
        let thread_builder = thread::Builder::new()
            .name("sender thread".into()) // 给线程起个有意义的名字，方便调试
            .stack_size(THREAD_STACK_SIZE);
        let codec_clone = Arc::clone(&codec_arc);

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

        let audio_packet_queue_arc = Arc::clone(&self.audio_packet_queue);

        let inner_sender = self.inner_sender.clone();

        let audio_processor = Arc::clone(&self.audio_processor);

        audio_processor
            .lock()
            .unwrap()
            .on_output(Box::new(move |data| {
                // println!("Received audio data: {:?}", data);
                let encoder = Arc::clone(&opus_encoder);
                let sender = inner_sender.clone();

                encoder
                    .lock()
                    .unwrap()
                    .encode(data, &mut move |opus_data: Vec<u8>| {
                        // info!("编码完成，add audio packet to queue");
                        let packet = AudioStreamPacket {
                            sample_rate: 16000,
                            frame_duration: 60,
                            timestamp: 0,
                            payload: opus_data,
                        };
                        sender.send(XzEvent::AddAudioPacketToQueue(packet)).unwrap();
                    })
                    .unwrap();
            }));

        let _ = thread_builder.spawn(move || {
            audio_loop(codec_clone, audio_processor);
        });

        info!("启动解码线程 start_output_audio ...");
        self.start_output_audio();

        self.set_device_state(DeviceState::Idle);

        info!("开始处理内部事件 ...");
        // 处理内部事件
        loop {
            match self.inner_receiver.recv() {
                Ok(event) => {
                    match event {
                        XzEvent::BootButtonClicked => {
                            info!("Boot button clicked! current state: {:?}", self.state);
                            if self.state == DeviceState::Starting
                                && !self.board.get_wifi_driver().is_connected().unwrap_or(false)
                            {
                                // TODO: 重置WiFi配置
                                // self.reset_wifi_configuration();
                            }
                            self.toggle_device_state();
                        }
                        XzEvent::VolumeButtonClicked => {
                            info!("Volume button clicked!");
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
                        XzEvent::WebSocketClosed => {
                            info!("WebSocketClosed");
                            // board.SetPowerSaveMode(true);
                            // Schedule([this]() {
                            //     auto display = Board::GetInstance().GetDisplay();
                            //     display->SetChatMessage("system", "");
                            //     SetDeviceState(kDeviceStateIdle);
                            // }); });
                            self.set_device_state(DeviceState::Idle);
                        }
                        XzEvent::WebsocketTextMessageReceived(text) => {
                            info!("Received text message: {}", text);
                            let message: serde_json::Value = serde_json::from_str(&text).unwrap();
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
                                                self.set_device_state(DeviceState::Speaking);
                                            }
                                        } else if state == "stop" {
                                            // TODO:: 看一下 background_task_ 在我们这里怎么实现，他的作用应该是等后台任务完成。
                                            // background_task_->WaitForCompletion();
                                            if self.state == DeviceState::Speaking {
                                                if self.listening_mode == ListeningMode::Manual {
                                                    self.set_device_state(DeviceState::Idle);
                                                } else {
                                                    self.set_device_state(DeviceState::Listening);
                                                }
                                            }
                                        }
                                        // TODO:: 处理其它文本
                                    }
                                }
                            }
                        }
                        XzEvent::SendAudioEvent => {
                            // info!("XzEvent::SendAudioEvent");
                            let packets = {
                                let mut queue = audio_packet_queue_arc.lock().unwrap();
                                // std::mem::take 会把 queue 换成默认值（空），并把原来的值返回
                                // 这完全等同于 C++ 的 std::move
                                std::mem::take(&mut *queue)
                            };

                            // 此时锁已经释放了
                            for packet in packets {
                                self.protocol.send_audio(&packet)?;
                            }
                        }
                        XzEvent::AudioPacketReceived(audio_stream_packet) => {
                            // 处理从服务器端接收到的音频数据包
                            info!("XzEvent::AudioPacketReceived - 从服务器端接收到的音频数据包");
                            if self.state == DeviceState::Speaking
                                && audio_packet_queue_arc.lock().unwrap().len()
                                    < MAX_AUDIO_PACKETS_IN_QUEUE
                            {
                                let mut audio_decode_queue =
                                    self.audio_decode_queue.lock().unwrap();
                                audio_decode_queue.push_back(audio_stream_packet);
                                self.decode_task_sender
                                    .send(XzEvent::AudioDecodeEvent)
                                    .unwrap();
                            }
                        }
                        XzEvent::AddAudioPacketToQueue(packet) => {
                            // info!("XzEvent::AddAudioPacketToQueue: add audio packet to queue");
                            // 把编码后的音频包添加待发送队列
                            let audio_packet_queue = Arc::clone(&audio_packet_queue_arc);
                            let mut queue = audio_packet_queue.lock().unwrap();

                            // --- 核心逻辑在这里 ---
                            // 2. 检查队列是否已满
                            if queue.len() >= MAX_AUDIO_PACKETS_IN_QUEUE {
                                warn!("Too many audio packets in queue, drop the newest packet");
                                continue;
                            }

                            // 4. 将新元素推入队列的尾部
                            queue.push_back(packet);
                            self.inner_sender
                                .clone()
                                .send(XzEvent::SendAudioEvent)
                                .unwrap();
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
        Ok(())
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
        //把下面的 C ++ 代码转换为 Rust 代码
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
        // 启动一个线程来decode audio数据
        const THREAD_STACK_SIZE: usize = 16 * 1024;
        let thread_builder = thread::Builder::new()
            .name("sender thread".into()) // 给线程起个有意义的名字，方便调试
            .stack_size(THREAD_STACK_SIZE);
        let codec = Arc::clone(&self.board.get_audio_codec());

        let sample_rate = 16000; //# 采样率固定为16000Hz
        let channels = 2; //# 单声道
        info!("create opus encoder");

        let audio_packet_queue_arc = Arc::clone(&self.audio_decode_queue);

        if let Some(rx) = self.decode_task_receiver.take() {
            let _ = thread_builder.spawn(move || {
                let mut opus_decoder = OpusAudioDecoder::new(sample_rate, channels).unwrap();
                let mut shared_pcm_buffer: Vec<i16> = Vec::with_capacity(4096);
                loop {
                    match rx.recv() {
                        Ok(event) => match event {
                            XzEvent::AudioDecodeEvent => {
                                let packets = {
                                    let mut queue = audio_packet_queue_arc.lock().unwrap();
                                    // std::mem::take 会把 queue 换成默认值（空），并把原来的值返回
                                    // 这完全等同于 C++ 的 std::move
                                    std::mem::take(&mut *queue)
                                };

                                // 此时锁已经释放了
                                for packet in packets {
                                    // self.protocol.send_audio(&packet).unwrap();
                                    decode_opus_audio(
                                        codec.clone(),
                                        &mut opus_decoder,
                                        packet.payload,
                                        &mut shared_pcm_buffer,
                                    );
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
                // info!("application start 函数返回！");
            });
        } else {
            println!("Receiver already taken!");
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

    const READ_CHUNK_SIZE: usize = 1024;
    let mut read_buffer = vec![0u8; READ_CHUNK_SIZE];
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
    }
}

fn start_audio_output(
    codec_arc: Arc<Mutex<dyn AudioCodec + 'static>>,
    audio_processor: Arc<Mutex<dyn AudioProcessor + 'static>>,
) {
    todo!()
}

fn start_audio_input(
    codec: Arc<Mutex<dyn AudioCodec + 'static>>,
    audio_processor: Arc<Mutex<dyn AudioProcessor + 'static>>,
    mut read_buffer: &mut Vec<u8>,
) {
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
    if audio_processor.lock().unwrap().is_running() {
        let samples = audio_processor.lock().unwrap().get_feed_size();
        // let mut data = vec![0; samples];
        let codec_arc = Arc::clone(&codec);

        if samples > 0 {
            let bytes_read = codec_arc
                .lock()
                .unwrap()
                .read_audio_data(&mut read_buffer)
                .unwrap();

            // 因为录音数据是8位PCM数据，opus_encoder 需要16位的 Vec,所以需转换下。
            let bytes_to_i16_result = bytes_to_i16_slice(&read_buffer[..bytes_read]).unwrap();

            audio_processor.lock().unwrap().feed(&bytes_to_i16_result);
        }
    }
    thread::sleep(Duration::from_millis((OPUS_FRAME_DURATION_MS / 2) as u64));
}

fn decode_opus_audio(
    codec: Arc<Mutex<dyn AudioCodec + 'static>>,
    opus_decoder: &mut OpusAudioDecoder,
    // mut i2s_driver: MutexGuard<'_, I2sDriver<'_, I2sBiDir>>,
    opus_data: Vec<u8>,
    pcm_buffer: &mut Vec<i16>,
) {
    // let sample_rate = 16000; //# 采样率固定为16000Hz
    let channels = 2; //# 双声道

    let decode_result = opus_decoder.decode(&opus_data);

    // let mut decoder = Box::new(OpusAudioDecoder::new(sample_rate, channels).unwrap());
    // let decode_result = decoder.decode(&opus_data);

    match decode_result {
        Ok(pcm_data) => {
            info!("decode success.");
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
                codec.lock().unwrap().output_data(pcm_stereo_bytes).unwrap();
                // play_pcm_audio(i2s_driver, pcm_stereo_bytes);
            } else {
                let pcm_stereo_bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        pcm_data.as_ptr() as *const u8,
                        pcm_data.len() * std::mem::size_of::<i16>(),
                    )
                };
                codec.lock().unwrap().output_data(pcm_stereo_bytes).unwrap();
            }
        }
        Err(e) => {
            info!("Opus decode error: {:?}", e);
            return;
        }
    }
}
