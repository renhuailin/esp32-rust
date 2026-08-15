pub mod lcd;

pub trait Display {
    fn set_status(&mut self, status: &str);
    fn show_qrcode(&mut self, content: &str);
    fn show_wifi_signal(&mut self, rssi: Option<i8>); // 显示wifi信号
    fn show_battery_level(&mut self, level: u8); // 显示电池电量
}
