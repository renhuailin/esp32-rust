use std::{
    collections::VecDeque,
    sync::{
        mpsc::{channel, Receiver, Sender},
        Arc, Mutex, MutexGuard,
    },
    thread,
};

use anyhow::{Error, Result};
use esp_idf_hal::{
    i2c::I2cDriver,
    i2s::{I2sBiDir, I2sDriver},
    peripheral,
};
use esp_idf_svc::{eventloop::EspSystemEventLoop, wifi::WifiDeviceId};
use log::{error, info, warn};

use crate::{
    audio::{
        codec::{
            audio_codec::AudioCodec, opus::encoder::OpusAudioEncoder, AudioStreamPacket,
            MAX_AUDIO_PACKETS_IN_QUEUE, OPUS_FRAME_DURATION_MS,
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
    device_id: String,
    board: Box<dyn Board<WifiDriver = Esp32WifiDriver>>,
    inner_sender: Sender<XzEvent>,
    inner_receiver: Receiver<XzEvent>,
    aec_mode: AecMode,
    listening_mode: ListeningMode,
    audio_processor: Arc<Mutex<dyn AudioProcessor>>,
    audio_packet_queue: Arc<Mutex<VecDeque<AudioStreamPacket>>>,
}

impl Application {
    pub fn new() -> Result<Self> {
        let (inner_sender, inner_receiver): (Sender<XzEvent>, Receiver<XzEvent>) = channel();

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
        let protocol = WebSocketProtocol::new(mac_address.as_str());

        //待发送的音频队列
        let audio_packet_queue = Arc::new(Mutex::new(
            VecDeque::<AudioStreamPacket>::with_capacity(MAX_AUDIO_PACKETS_IN_QUEUE),
        ));

        let instance = Self {
            state: DeviceState::Idle,
            protocol,
            device_id: mac_address,
            board,
            inner_sender,
            inner_receiver,
            aec_mode: AecMode::Off,
            listening_mode: ListeningMode::AutoStop,
            audio_processor: Arc::new(Mutex::new(NoAudioProcessor::new(16000))),
            audio_packet_queue,
        };

        Ok(instance)
    }

    pub fn start(&mut self) -> Result<(), Error> {
        self.set_device_state(DeviceState::Starting);
        let codec_arc = self.board.get_audio_codec();
        // let codec_arc = Arc::new(Mutex::new(codec));

        // let codec = self.board.get_audio_codec();
        codec_arc.lock().unwrap().start();

        let sender = self.inner_sender.clone();
        self.protocol.on_incoming_text(move |text| {
            info!("Received text message: {}", text);
            if let Err(e) = sender.send(XzEvent::WebsocketTextMessageReceived(text.to_string())) {
                log::error!("Failed to send WebsocketTextMessageReceived event: {:?}", e);
            }
            Ok(())
        })?;

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
                println!("Received audio data: {:?}", data);
                let encoder = Arc::clone(&opus_encoder);
                let sender = inner_sender.clone();

                encoder
                    .lock()
                    .unwrap()
                    .encode(data, &mut move |opus_data: Vec<u8>| {
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

        info!("开始处理内部事件 ...");
        // 处理内部事件
        loop {
            match self.inner_receiver.recv() {
                Ok(event) => {
                    match event {
                        XzEvent::BootButtonClicked => {
                            if self.state == DeviceState::Idle
                                && self.board.get_wifi_driver().is_connected().unwrap_or(false)
                            {
                                self.toggle_device_state();
                            }
                        }
                        XzEvent::VolumeButtonClicked => {
                            info!("Volume button clicked");
                        }
                        XzEvent::WebSocketConnected => {
                            info!("Connected,try to send hello message");
                            // // send client hello message
                            // if let Some(client) = &mut self.client {
                            //     if client.is_connected() {
                            //         let hello_message = ClientHelloMessage::new().unwrap();
                            //         info!("Worker thread: Sending hello message...");
                            //         match client.send(FrameType::Text(false), hello_message.as_bytes()) {
                            //             Ok(_) => info!("Worker thread: Hello message sent!"),
                            //             Err(e) => info!("Worker thread: Send error: {:?}", e),
                            //         }
                            //     } else {
                            //         info!("Worker thread: Client not connected, cannot send.");
                            //     }
                            // }
                        }
                        XzEvent::WebsocketTextMessageReceived(text) => {
                            info!("Received text message: {}", text);
                        }
                        // XzEvent::ServerHelloMessageReceived => {
                        //     info!("Worker thread: Server hello message received.");
                        // }
                        XzEvent::SendAudioEvent => {
                            info!("SendAudioEvent not implemented yet");

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
                        // XzEvent::AudioDataReceived(audio_stream_packet) => {
                        //     info!("AudioDataReceived not implemented yet");
                        // }
                        XzEvent::AddAudioPacketToQueue(packet) => {
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
            DeviceState::Idle => todo!(),
            DeviceState::Activating => todo!(),
            DeviceState::WifiConfiguring => todo!(),
            DeviceState::Connecting => todo!(),
            // DeviceState::DeviceStateAudioTesting => todo!(),
            DeviceState::Speaking => todo!(),
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
            DeviceState::Starting => todo!(),
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
                // self.protocol.close_audio_channel().unwrap();
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
}
