//! 轻量 GUI 层：基于 embedded-graphics 0.8，状态与绘制分离。
//!
//! 设计要点（与现有 event loop 兼容）：
//! - `GuiState` 是 UI 数据快照，渲染函数以它为输入，单向数据流；
//! - `Display` trait 的命令式调用（set_status 等）同步更新状态并立即
//!   重绘对应区域，全部发生在 Application 主循环线程，无独立 GUI 线程、
//!   无独立 tick 源，与 `recv_timeout(BACKLIGHT_TICK)` 事件循环天然共存；
//! - 将来需要动画（呼吸提示、消息滚动）时，在主循环
//!   `update_backlight()` 同款位置挂 `display.tick()` 即可（250ms 节拍）。

pub mod chat;
pub mod status_bar;

pub use chat::ChatMessage;

use crate::display::BatteryStatus;

/// UI 状态快照。
#[derive(Debug, Clone)]
pub struct GuiState {
    /// 顶栏状态文字（也用于内容区状态大字）
    pub status: String,
    /// WiFi 信号强度（None = 未连接）
    pub wifi_rssi: Option<i8>,
    /// 电池状态（电量/充电/USB）
    pub battery: BatteryStatus,
    /// 最近一条聊天消息（user / assistant / system）
    pub chat_message: Option<ChatMessage>,
    /// 二维码模式：内容区被配置二维码占用，状态文字不再覆盖内容区。
    /// 配网流程靠重启退出，故无需复位。
    pub qrcode_active: bool,
}

impl GuiState {
    pub fn new() -> Self {
        Self {
            status: String::new(),
            wifi_rssi: None,
            battery: BatteryStatus::default(),
            chat_message: None,
            qrcode_active: false,
        }
    }
}

impl Default for GuiState {
    fn default() -> Self {
        Self::new()
    }
}
