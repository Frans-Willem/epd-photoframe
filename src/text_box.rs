use alloc::vec::Vec;

use embedded_graphics::Drawable;
use embedded_graphics::mono_font::MonoFont;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::prelude::{Point, Primitive, Size};
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_text::TextBox;

use crate::canvas::Canvas;
use crate::panel::PanelColor;

const BOX_PADDING_PX: u32 = 24;

pub fn draw_centered<C: PanelColor>(canvas: &mut Canvas<C>, width: u32, height: u32, text: &str) {
    let text_width = text_width_px(text, &FONT_10X20);
    let text_height = text_height_px(text, &FONT_10X20);
    let max_text_width = width.saturating_sub(2 * BOX_PADDING_PX);
    let text_width = text_width.min(max_text_width);

    let box_width = (text_width + 2 * BOX_PADDING_PX).min(width);
    let box_height = (text_height + 2 * BOX_PADDING_PX).min(height);
    let box_x = ((width - box_width) / 2) as i32;
    let box_y = ((height - box_height) / 2) as i32;
    let box_area = Rectangle::new(Point::new(box_x, box_y), Size::new(box_width, box_height));

    let _ = box_area
        .into_styled(PrimitiveStyle::with_fill(C::WHITE))
        .draw(canvas);
    let _ = box_area
        .into_styled(PrimitiveStyle::with_stroke(C::BLACK, 2))
        .draw(canvas);

    let text_area = Rectangle::new(
        Point::new(box_x + BOX_PADDING_PX as i32, box_y + BOX_PADDING_PX as i32),
        Size::new(
            box_width.saturating_sub(2 * BOX_PADDING_PX),
            box_height.saturating_sub(2 * BOX_PADDING_PX),
        ),
    );
    let style = MonoTextStyle::new(&FONT_10X20, C::BLACK);
    let _ = TextBox::new(text, text_area, style).draw(canvas);
}

pub fn draw_centered_on_frame<C: PanelColor>(
    frame: Vec<C>,
    width: u32,
    height: u32,
    text: &str,
) -> Vec<C> {
    let mut canvas = Canvas::from_vec(width, height, frame);
    draw_centered(&mut canvas, width, height, text);
    canvas.into_vec()
}

fn text_width_px(text: &str, font: &MonoFont<'_>) -> u32 {
    let char_width = font.character_size.width;
    text.lines()
        .map(|line| line.chars().count() as u32 * char_width)
        .max()
        .unwrap_or(0)
}

fn text_height_px(text: &str, font: &MonoFont<'_>) -> u32 {
    let line_count = text.lines().count().max(1) as u32;
    line_count * font.character_size.height
}
