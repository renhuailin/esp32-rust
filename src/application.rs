use std::{
    collections::VecDeque,
    sync::{mpsc::Sender, MutexGuard},
};

use anyhow::{Error, Result};
use esp_idf_hal::i2s::{I2sBiDir, I2sDriver};
use log::info;

use crate::{common::event::XzEvent, protocols::websocket::ws_protocol::WebSocketProtocol};

pub enum ApplicationState {
    Idle,
    Playing,
    Recording,
}

pub struct ApplicationConfig {
    device_id: String,
    sender: Sender<XzEvent>,
}

pub struct Application {
    state: ApplicationState,
    protocol: WebSocketProtocol,
}

impl Application {
    pub fn new(config: ApplicationConfig) -> Self {
        let protocol = WebSocketProtocol::new(&config.device_id, config.sender);

        Self {
            state: ApplicationState::Idle,
            protocol,
        }
    }

    pub fn start() {}

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
}
