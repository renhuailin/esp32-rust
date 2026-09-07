//! 顶栏组件：WiFi 信号 / 状态文字 / 电池电量。
//! 绘制逻辑自 st7789.rs 平移；逻辑屏 320x240，顶栏高 40px。

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Line, PrimitiveStyleBuilder, Rectangle},
    text::Text,
};
use u8g2_fonts::U8g2TextStyle;

use crate::display::BatteryStatus;

/// 顶栏高度（px）
pub const BAR_HEIGHT: i32 = 40;

fn clear_rect<D>(target: &mut D, x: i32, y: i32, w: u32, h: u32)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    Rectangle::new(Point::new(x, y), Size::new(w, h))
        .into_styled(PrimitiveStyleBuilder::new().fill_color(Rgb565::BLACK).build())
        .draw(target)
        .unwrap();
}

/// 状态文字（顶栏中部 x=60..260，wqy16 居中）
pub fn draw_status_text<D>(target: &mut D, status: &str)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    clear_rect(target, 60, 0, 200, BAR_HEIGHT as u32);

    let style = U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312, Rgb565::WHITE);

    let mut text_width = 0;
    for ch in status.chars() {
        text_width += if ch.is_ascii() { 8 } else { 16 };
    }
    let x = 60 + (200 - text_width) / 2;
    let x = x.max(60).min(240);

    Text::new(status, Point::new(x, 24), style)
        .draw(target)
        .unwrap();
}

/// WiFi 信号（顶栏左侧 x=0..60，4 格信号条，按 rssi 着色）
pub fn draw_wifi_signal<D>(target: &mut D, rssi: Option<i8>)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    clear_rect(target, 0, 0, 60, BAR_HEIGHT as u32);

    let bars = match rssi {
        None => 0,
        Some(r) if r >= -30 => 4,
        Some(r) if r >= -50 => 3,
        Some(r) if r >= -70 => 2,
        Some(r) if r >= -85 => 1,
        Some(_) => 0,
    };

    let bar_w: i32 = 6;
    let gap: i32 = 2;
    let base_x: i32 = 5;
    let base_y: i32 = 24; // 底部对齐在 y=24
    let max_h: i32 = 16;

    for i in 0..4 {
        let h = (i + 1) * max_h / 4;
        let x = base_x + i * (bar_w + gap);
        let y = base_y - h;

        let color = if i < bars {
            match bars {
                1 | 2 => Rgb565::RED,
                3 => Rgb565::YELLOW,
                _ => Rgb565::GREEN,
            }
        } else {
            Rgb565::new(0x20, 0x20, 0x20)
        };

        Rectangle::new(Point::new(x, y), Size::new(bar_w as u32, h as u32))
            .into_styled(PrimitiveStyleBuilder::new().fill_color(color).build())
            .draw(target)
            .unwrap();
    }
}

/// 电池电量（顶栏右侧 x=260..320，30x14 外框 + 电量填充）
pub fn draw_battery_level<D>(target: &mut D, level: u8)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    clear_rect(target, 260, 0, 60, BAR_HEIGHT as u32);

    let level = level.min(100);

    let bat_x = 275;
    let bat_y = 11;
    let bat_w = 30;
    let bat_h = 14;

    Rectangle::new(Point::new(bat_x, bat_y), Size::new(bat_w, bat_h))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Rgb565::WHITE)
                .stroke_width(1)
                .fill_color(Rgb565::BLACK)
                .build(),
        )
        .draw(target)
        .unwrap();

    Rectangle::new(
        Point::new(bat_x + bat_w as i32, bat_y + 4),
        Size::new(3, 6),
    )
    .into_styled(PrimitiveStyleBuilder::new().fill_color(Rgb565::WHITE).build())
    .draw(target)
    .unwrap();

    if level > 0 {
        let max_fill = (bat_w - 4) as u32;
        let fill_w = (max_fill * level as u32 / 100).max(1);
        let fill_color = if level > 60 {
            Rgb565::GREEN
        } else if level > 20 {
            Rgb565::YELLOW
        } else {
            Rgb565::RED
        };

        Rectangle::new(
            Point::new(bat_x + 2, bat_y + 2),
            Size::new(fill_w, (bat_h - 4) as u32),
        )
        .into_styled(PrimitiveStyleBuilder::new().fill_color(fill_color).build())
        .draw(target)
        .unwrap();
    }
}

/// 电池完整状态（顶栏右侧 x=260..320）：
/// 电量填充 + 充电闪电 + 充电完成指示。
/// - 充电中：黄色填充
/// - 充电完成：绿色填充
/// - USB 插入（VBUS 在位）：电池左侧显示竖直闪电图标（与手机效果一致）
pub fn draw_battery_status<D>(target: &mut D, status: &BatteryStatus)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    clear_rect(target, 260, 0, 60, BAR_HEIGHT as u32);

    // 电池本体（复用现有绘制逻辑，重画一遍）
    draw_battery_level_raw(target, status.level, status.charging, status.charge_done);

    // USB 插入或充电中：电池左侧画竖直白色闪电（Z 形折线，3 段）
    if status.vbus_present || status.charging {
        let style = PrimitiveStyleBuilder::new()
            .stroke_color(Rgb565::WHITE)
            .stroke_width(1)
            .build();
        let pts = [
            (Point::new(270, 12), Point::new(264, 18)), // 左下斜
            (Point::new(264, 18), Point::new(269, 18)), // 中段右移
            (Point::new(269, 18), Point::new(263, 24)), // 尾段左下
        ];
        for (a, b) in pts {
            Line::new(a, b).into_styled(style).draw(target).unwrap();
        }
    }
}

/// 电池电量原始绘制（外框 + 填充），颜色由充电状态决定。
/// - 充电中：黄色填充
/// - 充电完成：绿色填充
/// - 其他：按电量红/黄/绿
fn draw_battery_level_raw<D>(target: &mut D, level: u8, charging: bool, charge_done: bool)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    let level = level.min(100);

    let bat_x = 275;
    let bat_y = 11;
    let bat_w = 30;
    let bat_h = 14;

    Rectangle::new(Point::new(bat_x, bat_y), Size::new(bat_w, bat_h))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Rgb565::WHITE)
                .stroke_width(1)
                .fill_color(Rgb565::BLACK)
                .build(),
        )
        .draw(target)
        .unwrap();

    Rectangle::new(
        Point::new(bat_x + bat_w as i32, bat_y + 4),
        Size::new(3, 6),
    )
    .into_styled(PrimitiveStyleBuilder::new().fill_color(Rgb565::WHITE).build())
    .draw(target)
    .unwrap();

    if level > 0 {
        let max_fill = (bat_w - 4) as u32;
        let fill_w = (max_fill * level as u32 / 100).max(1);
        let fill_color = if charging {
            Rgb565::YELLOW
        } else if charge_done {
            Rgb565::GREEN
        } else if level > 60 {
            Rgb565::GREEN
        } else if level > 20 {
            Rgb565::YELLOW
        } else {
            Rgb565::RED
        };

        Rectangle::new(
            Point::new(bat_x + 2, bat_y + 2),
            Size::new(fill_w, (bat_h - 4) as u32),
        )
        .into_styled(PrimitiveStyleBuilder::new().fill_color(fill_color).build())
        .draw(target)
        .unwrap();
    }
}
