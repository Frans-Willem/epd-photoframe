use alloc::string::String;
use alloc::vec::Vec;

use embedded_graphics::Drawable;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::prelude::Point;
use embedded_graphics::text::{Baseline, Text};

use crate::canvas::Canvas;
use crate::spectra6::Spectra6Color;

/// Render `message` as black text on a white Spectra 6 frame of the given
/// dimensions and return the frame as a row-major `Vec<Spectra6Color>`.
pub fn render(width: usize, height: usize, message: &str) -> Vec<Spectra6Color> {
    let mut canvas = Canvas::new(width as u32, height as u32);
    let style = MonoTextStyle::new(&FONT_10X20, Spectra6Color::Black);
    let char_width = FONT_10X20.character_size.width as usize;
    let max_chars = width.saturating_sub(20) / char_width.max(1);
    let wrapped = hard_wrap(message, max_chars);
    let _ = Text::with_baseline(&wrapped, Point::new(10, 10), style, Baseline::Top)
        .draw(&mut canvas);
    canvas.into_vec()
}

fn hard_wrap(s: &str, max_chars_per_line: usize) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    for c in s.chars() {
        if c == '\n' {
            out.push('\n');
            col = 0;
        } else {
            if col >= max_chars_per_line {
                out.push('\n');
                col = 0;
            }
            out.push(c);
            col += 1;
        }
    }
    out
}
