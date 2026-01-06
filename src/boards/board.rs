use anyhow::Error;

use crate::wifi::WifiStation;

// 定义主板的抽象
pub trait Board {
    // 关联类型：具体的 WiFi 驱动类型，只要它实现了 WifiStation
    type WifiDriver: WifiStation;

    // 获取该主板的 WiFi 驱动
    // fn get_wifi(&self) -> Self::WifiDriver;
    fn init_wifi(&mut self) -> Result<(), Error>;

    fn get_wifi_driver(&self) -> &Self::WifiDriver;

    fn on_touch_button_clicked(&mut self, on_clicked: Box<dyn FnMut() + Send + 'static>);
    fn on_volume_button_clicked(&mut self, on_clicked: Box<dyn FnMut() + Send + 'static>);

    // 你还可以加其他的，比如 Display
    // type DisplayDriver: DrawTarget;
    // fn get_display(&self) -> Self::DisplayDriver;
}
