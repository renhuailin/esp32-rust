pub mod audio;
pub mod axp173;
pub mod common;
// pub mod i2s;
pub mod lcd;
pub mod led;
pub mod protocols;
pub mod utils;
pub mod wifi;

use std::{collections::VecDeque, sync::MutexGuard};

use anyhow::{Error, Result};
use esp_idf_hal::i2s::{I2sBiDir, I2sDriver};
use log::info;

pub enum ApplicationState {
    Idle,
    Playing,
    Recording,
}

pub struct Application {
    state: ApplicationState,
}

impl Application {
    pub fn new() -> Self {
        Self {
            state: ApplicationState::Idle,
        }
    }

    // pub fn run(&mut self) {
    //     match self.state {
    //         ApplicationState::Idle => {}
    //         ApplicationState::Playing => {}
    //         ApplicationState::Recording => {}
    //     }
    // }

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
