use std::{
    collections::VecDeque,
    sync::{mpsc::Sender, MutexGuard},
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
    common::event::XzEvent,
    protocols::websocket::ws_protocol::WebSocketProtocol,
    wifi,
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
    device_id: String,
    sender: Sender<XzEvent>,
}

pub struct Application {
    state: ApplicationState,
    protocol: WebSocketProtocol,
    device_id: String,
}

impl Application {
    pub fn new(config: ApplicationConfig) -> Self {
        let protocol = WebSocketProtocol::new(&config.device_id, config.sender);

        Self {
            state: ApplicationState::Idle,
            protocol,
            device_id: "".to_string(),
        }
    }

    pub fn start(&mut self) {
        self.set_device_state(ApplicationState::Starting);
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
