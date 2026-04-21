use alloc::string::String;
use alloc::vec::Vec;
use core::convert::Infallible;

use embedded_graphics::Drawable;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::prelude::{DrawTarget, OriginDimensions, Pixel, Point, Size};
use embedded_graphics::text::{Baseline, Text};

use crate::spectra6::Spectra6Color;

struct Canvas<'a> {
    pixels: &'a mut [Spectra6Color],
    width: u32,
    height: u32,
}

impl<'a> OriginDimensions for Canvas<'a> {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

impl<'a> DrawTarget for Canvas<'a> {
    type Color = Spectra6Color;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let w = self.width as i32;
        let h = self.height as i32;
        let stride = self.width as usize;
        for Pixel(point, color) in pixels {
            if point.x >= 0 && point.y >= 0 && point.x < w && point.y < h {
                let idx = point.y as usize * stride + point.x as usize;
                self.pixels[idx] = color;
            }
        }
        Ok(())
    }
}

/// Render `message` as black text on a white Spectra 6 frame of the given
/// dimensions and return the frame as a row-major `Vec<Spectra6Color>`.
pub fn render(width: usize, height: usize, message: &str) -> Vec<Spectra6Color> {
    let mut pixels = alloc::vec![Spectra6Color::White; width * height];
    {
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width: width as u32,
            height: height as u32,
        };
        let style = MonoTextStyle::new(&FONT_10X20, Spectra6Color::Black);
        let char_width = FONT_10X20.character_size.width as usize;
        let max_chars = width.saturating_sub(20) / char_width.max(1);
        let wrapped = hard_wrap(message, max_chars);
        let _ = Text::with_baseline(&wrapped, Point::new(10, 10), style, Baseline::Top)
            .draw(&mut canvas);
    }
    pixels
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
