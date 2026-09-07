use std::sync::{Arc, Mutex};

use anyhow::{Error, Result};

use crate::{
    audio::codec::audio_codec::AudioCodec, display::{BatteryStatus, Display},
    wifi::wifi_driver::WifiStation,
};
// 定义主板的抽象
pub trait Board {
    // 关联类型：具体的 WiFi 驱动类型，只要它实现了 WifiStation
    type WifiDriver: WifiStation;
    type DisplayDriver: Display;

    // 获取该主板的 WiFi 驱动
    // fn get_wifi(&self) -> Self::WifiDriver;
    fn init_wifi(&mut self) -> Result<(), Error>;

    fn get_wifi_driver(&self) -> &Self::WifiDriver;

    fn on_speak_button_clicked(&mut self, on_clicked: Box<dyn FnMut() + Send + 'static>);
    fn on_volume_button_clicked(&mut self, on_clicked: Box<dyn FnMut() + Send + 'static>);
    fn on_volume_button_long_pressed(&mut self, on_clicked: Box<dyn FnMut() + Send + 'static>);

    fn set_on_wifi_connected_callback(
        &mut self,
        on_connected: Box<dyn FnMut(String, String) + Send + 'static>,
    );

    fn get_audio_codec(&mut self) -> Arc<Mutex<dyn AudioCodec>>;

    fn start_wifi_station(&mut self) -> Result<(bool, Vec<String>), Error>;

    fn start_wifi_ap(&mut self, available_ap_names: Vec<String>) -> Result<bool, Error>;

    fn start_network(&mut self) -> Result<()>;

    // 你还可以加其他的，比如 Display
    // type DisplayDriver: DrawTarget;
    fn get_display(&mut self) -> &mut Self::DisplayDriver;

    /// 读取电池状态（电量/充电/USB 插入），供状态栏定时刷新。
    /// 无电源管理芯片的实现保持默认（返回默认空状态）。
    fn read_battery_status(&mut self) -> Result<BatteryStatus> {
        Ok(BatteryStatus::default())
    }
}
