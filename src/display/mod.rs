pub mod lcd;

/// 电池状态快照（数据来源：AXP173）。
#[derive(Debug, Clone, Copy, Default)]
pub struct BatteryStatus {
    /// 电量百分比（0-100，电压折算）
    pub level: u8,
    /// 充电中
    pub charging: bool,
    /// 充电完成（电池在位且不在充电）
    pub charge_done: bool,
    /// 放电中（电池在位、无外部电源、电流方向为放电）
    pub discharging: bool,
    /// USB (VBUS) 已插入
    pub vbus_present: bool,
}

pub trait Display {
    fn set_status(&mut self, status: &str);
    fn show_qrcode(&mut self, content: &str);
    fn show_wifi_signal(&mut self, rssi: Option<i8>); // 显示wifi信号
    fn show_battery_level(&mut self, level: u8); // 显示电池电量
    /// 显示电池完整状态（电量 + 充电/USB 插入），覆盖 show_battery_level。
    /// 必须在 Application 主循环线程调用；协议线程请经 AppEvent 投递后再调。
    fn show_battery_status(&mut self, _status: &BatteryStatus) {}
    /// 显示一条聊天消息（role: "user" / "assistant" / "system"）。
    /// 必须在 Application 主循环线程调用；协议线程请经 AppEvent 投递后再调。
    fn set_chat_message(&mut self, _role: &str, _text: &str) {}
}
