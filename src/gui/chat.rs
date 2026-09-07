//! 内容区渲染：聊天消息（自动换行）与状态大字。
//! 内容区为顶栏以下区域（逻辑屏 320x240，y = 40..240）。

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
    text::Text,
};
use u8g2_fonts::U8g2TextStyle;

use super::status_bar::BAR_HEIGHT;

const SCREEN_W: i32 = 320;
const SCREEN_H: i32 = 240;
/// 行高（wqy16 字高 16 + 行距 4）
const LINE_HEIGHT: i32 = 20;
/// 内容区最多可见行数：(240 - 40 - 16) / 20
const MAX_LINES: usize = 9;

/// 一条聊天消息
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub text: String,
}

/// 单字符显示宽度（wqy16：ASCII 8px，CJK 16px）
pub fn char_width(ch: char) -> i32 {
    if ch.is_ascii() { 8 } else { 16 }
}

/// 文本显示宽度（px）
pub fn text_width(s: &str) -> i32 {
    s.chars().map(char_width).sum()
}

/// 贪心折行：中文逐字断行，英文尽量按空格断。
pub fn wrap_text(s: &str, max_width: i32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_w = 0;
    let mut last_space: Option<usize> = None; // 当前行内最后空格的字节位置

    for ch in s.chars() {
        let w = char_width(ch);
        if ch == '\n' {
            lines.push(std::mem::take(&mut line));
            line_w = 0;
            last_space = None;
            continue;
        }
        if line_w + w > max_width && !line.is_empty() {
            if let Some(pos) = last_space {
                let rest: String = line[pos + 1..].to_string();
                line.truncate(pos);
                lines.push(std::mem::take(&mut line));
                line = rest;
                line_w = text_width(&line);
            } else {
                lines.push(std::mem::take(&mut line));
                line_w = 0;
            }
            last_space = None;
        }
        if ch == ' ' {
            last_space = Some(line.len());
        }
        line.push(ch);
        line_w += w;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// 清空内容区（顶栏以下整条）
pub fn clear_content<D>(target: &mut D)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    Rectangle::new(
        Point::new(0, BAR_HEIGHT),
        Size::new(SCREEN_W as u32, (SCREEN_H - BAR_HEIGHT) as u32),
    )
    .into_styled(PrimitiveStyleBuilder::new().fill_color(Rgb565::BLACK).build())
    .draw(target)
    .unwrap();
}

/// 绘制聊天消息：自动换行，超出保留尾部（滚动到最新）。
/// role 决定颜色：user 白 / assistant 青 / 其他灰。
pub fn draw_chat_message<D>(target: &mut D, msg: &ChatMessage)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    clear_content(target);

    let color = match msg.role.as_str() {
        "user" => Rgb565::WHITE,
        "assistant" => Rgb565::CYAN,
        _ => Rgb565::new(0xA0, 0xA0, 0xA0),
    };

    let mut lines = wrap_text(&msg.text, SCREEN_W - 16);
    if lines.len() > MAX_LINES {
        let tail = lines.split_off(lines.len() - MAX_LINES);
        lines = tail;
    }

    let mut y = BAR_HEIGHT + 8 + 16; // 首行 baseline
    for l in &lines {
        let style = U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312, color);
        Text::new(l, Point::new(8, y), style).draw(target).unwrap();
        y += LINE_HEIGHT;
    }
}

/// 状态大字：单行居中显示在内容区（超宽截断）。
/// 供 set_status 同步刷新，让设备状态变化有明显视觉落点。
pub fn draw_centered_state<D>(target: &mut D, text: &str)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    clear_content(target);

    let mut chars: Vec<char> = text.chars().collect();
    while chars.iter().map(|c| char_width(*c)).sum::<i32>() > SCREEN_W - 32 && chars.len() > 1 {
        chars.pop();
    }
    let shown: String = chars.into_iter().collect();

    let w = text_width(&shown);
    let x = ((SCREEN_W - w) / 2).max(0);
    let y = BAR_HEIGHT + (SCREEN_H - BAR_HEIGHT + 16) / 2; // 视觉居中（含字高）

    let style = U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312, Rgb565::WHITE);
    Text::new(&shown, Point::new(x, y), style).draw(target).unwrap();
}
