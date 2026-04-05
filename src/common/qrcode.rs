use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
};
use qrcode::{Color, QrCode};

///
///module_size 决定了二维码中的每一个“逻辑点”（Module）在你的屏幕上用几个像素（Pixel）来显示
pub fn draw_qrcode<D>(
    target: &mut D,
    content: &str,
    top_left: Point,
    module_size: u32,
    foreground: Rgb565, // 前景色（通常是黑）
    background: Rgb565, // 背景色（通常是白）
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    // 1. 使用 qrcode 库生成矩阵
    let code = QrCode::new(content).unwrap();
    let width = code.width() as u32;

    // 2. 遍历矩阵中的每一个点
    for y in 0..width {
        for x in 0..width {
            let color = match code[(x as usize, y as usize)] {
                Color::Dark => foreground,
                Color::Light => background,
            };

            let x_pos = top_left.x + (x * module_size) as i32;
            let y_pos = top_left.y + (y * module_size) as i32;

            Rectangle::new(
                Point::new(x_pos, y_pos),
                Size::new(module_size, module_size),
            )
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(target)?;
        }
    }
    Ok(())
}
