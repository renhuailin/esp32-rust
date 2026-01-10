use std::{
    collections::VecDeque,
    sync::{
        mpsc::{channel, Receiver, Sender},
        MutexGuard, OnceLock,
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
use log::{error, info};

use crate::{
    audio::{
        codec::audio_codec::AudioCodec,
        processor::{audio_processor::AudioProcessor, no_audio_processor::NoAudioProcessor},
    },
    axp173::{Axp173, Ldo},
    boards::{board::Board, jianglian_s3cam_board},
    common::{
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
    audio_processor: NoAudioProcessor,
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

        let instance = Self {
            state: DeviceState::Idle,
            protocol,
            device_id: mac_address,
            board,
            inner_sender,
            inner_receiver,
            aec_mode: AecMode::Off,
            listening_mode: ListeningMode::AutoStop,
        };

        Ok(instance)
    }

    pub fn start(&mut self) -> Result<(), Error> {
        self.set_device_state(DeviceState::Starting);

        let codec = self.board.get_audio_codec();
        codec.start();

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
        let codec = self.board.get_audio_codec();
        thread_builder.spawn(move || {
            audio_loop(codec);
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
                        // XzEvent::SendAudioEvent => {
                        //     info!("SendAudioEvent not implemented yet");
                        // }
                        // XzEvent::AudioDataReceived(audio_stream_packet) => {
                        //     info!("AudioDataReceived not implemented yet");
                        // }
                        _ => {
                            info!("Received unhandled event: {:?}", event);
                        }
                    }
                }
                Err(_) => {
                    info!("Event channel closed, exiting event loop");
                    break;
                }
            }
        }
        info!("application start 函数返回！");
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
                if !self.audio_processor.is_running() {
                    self.protocol
                        .send_start_linstening(self.listening_mode.clone());
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
                    self.audio_processor.start();
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
        //把下面的 C++ 代码转换为 Rust 代码
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

fn audio_loop(codec: &mut dyn AudioCodec) {}
