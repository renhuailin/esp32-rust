use std::{
    collections::VecDeque,
    sync::{
        mpsc::{channel, Receiver, Sender},
        MutexGuard, OnceLock,
    },
};

use anyhow::{Error, Result};
use esp_idf_hal::{
    i2c::I2cDriver,
    i2s::{I2sBiDir, I2sDriver},
    peripheral,
};
use esp_idf_svc::{eventloop::EspSystemEventLoop, wifi::WifiDeviceId};
use log::info;

use crate::{
    axp173::{Axp173, Ldo},
    boards::{board::Board, jianglian_s3cam_board},
    common::event::XzEvent,
    protocols::{protocol::Protocol, websocket::ws_protocol::WebSocketProtocol},
    wifi::{self, Esp32WifiDriver, WifiStation},
};
use shared_bus::{BusManager, BusManagerSimple};

#[derive(PartialEq)]
pub enum ApplicationState {
    Idle,
    Playing,
    Recording,
    Starting,
}

pub struct ApplicationConfig {
    sender: Sender<XzEvent>,
}

pub struct Application {
    state: ApplicationState,
    protocol: WebSocketProtocol,
    device_id: String,
    board: Box<dyn Board<WifiDriver = Esp32WifiDriver>>,
    inner_sender: Sender<XzEvent>,
    inner_receiver: Receiver<XzEvent>,
}

impl Application {
    pub fn new() -> Self {
        let (inner_sender, inner_receiver): (Sender<XzEvent>, Receiver<XzEvent>) = channel();

        let mut board = Box::new(jianglian_s3cam_board::JiangLianS3CamBoard::new().unwrap());

        board.on_touch_button_clicked(Box::new(move || info!("Touch button clicked")));
        board.on_volume_button_clicked(Box::new(move || info!("Volume button clicked")));

        board.init().unwrap();
        let mac_address = board.get_wifi_driver().get_mac_address().unwrap();
        let protocol = WebSocketProtocol::new(mac_address.as_str());

        let instance = Self {
            state: ApplicationState::Idle,
            protocol,
            device_id: mac_address,
            board,
            inner_sender,
            inner_receiver,
        };

        instance
    }

    pub fn start(&mut self) -> Result<(), Error> {
        self.set_device_state(ApplicationState::Starting);
        self.protocol.on_incoming_text(|text| {
            info!("Received text message: {}", text);
            Ok(())
        })?;

        // 处理内部事件
        for event in &self.inner_receiver {
            match event {
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
                // XzEvent::ServerHelloMessageReceived => {
                //     info!("Worker thread: Server hello message received.");
                //     break;
                // }
                // XzEvent::SendAudioEvent => todo!(),
                // XzEvent::AudioDataReceived(audio_stream_packet) => todo!(),
                _ => todo!(),
            }
        }

        Ok(())
    }

    pub fn read_audio(
        &mut self,
        mut i2s_driver: MutexGuard<'_, I2sDriver<'_, I2sBiDir>>,
        mut buffer: std::sync::MutexGuard<'_, VecDeque<i16>>,
    ) -> Result<(), Error> {
        // 读取音频数据

        // match self.state {
        //     ApplicationState::Idle => {}
        //     ApplicationState::Playing => {}
        //     ApplicationState::Recording => {}
        // }
        self.state = ApplicationState::Recording;
        info!("Reading audio...");
        // i2s_driver.read(buffer, timeout);

        Ok(())
    }

    // private methods
    fn set_device_state(&mut self, state: ApplicationState) {
        if self.state == state {
            return;
        }
        self.state = state;
    }
}
