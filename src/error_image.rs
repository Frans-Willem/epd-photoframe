use alloc::string::String;
use alloc::vec::Vec;

use embedded_graphics::Drawable;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::prelude::Point;
use embedded_graphics::text::{Baseline, Text};

use crate::canvas::Canvas;
use crate::spectra6::Spectra6Color;

/// Canonical recovery hint appended to every rendered error frame, so
/// the user always has a way back into config mode regardless of which
/// failure they're looking at (WiFi, HTTP, PNG, OOM, etc.).
const RECONFIGURE_HINT: &str =
    "To reconfigure, hold Previous+Next for 10 seconds during the next boot.";

/// Render `message` as black text on a white Spectra 6 frame of the given
/// dimensions and return the frame as a row-major `Vec<Spectra6Color>`.
/// Appends the reconfigure hint below the caller's message — callers
/// should supply only the failure-specific part.
pub fn render(width: usize, height: usize, message: &str) -> Vec<Spectra6Color> {
    let mut canvas = Canvas::new(width as u32, height as u32);
    let style = MonoTextStyle::new(&FONT_10X20, Spectra6Color::Black);
    let char_width = FONT_10X20.character_size.width as usize;
    let max_chars = width.saturating_sub(20) / char_width.max(1);
    let full = {
        let mut s = String::from(message);
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s.push('\n');
        s.push_str(RECONFIGURE_HINT);
        s
    };
    let wrapped = hard_wrap(&full, max_chars);
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
