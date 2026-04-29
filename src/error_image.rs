use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use embassy_time::Duration;
use embedded_graphics::Drawable;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::prelude::{Point, Size};
use embedded_graphics::primitives::Rectangle;
use embedded_text::TextBox;

use crate::canvas::Canvas;
use crate::spectra6::Spectra6Color;

const MARGIN_PX: u32 = 24;

/// Canonical recovery hint appended to every rendered error frame, so
/// the user always has a way back into config mode regardless of which
/// failure they're looking at (WiFi, HTTP, PNG, OOM, etc.).
const RECONFIGURE_HINT: &str =
    "To reconfigure, hold Previous+Next for 10 seconds during the next boot.";

/// Render `message` as black text on a white Spectra 6 frame of the given
/// dimensions and return the frame as a row-major `Vec<Spectra6Color>`.
/// Appends a "Will retry in …" line plus the reconfigure hint below the
/// caller's message — callers should supply only the failure-specific
/// part.
pub fn render(
    width: usize,
    height: usize,
    message: &str,
    retry_in: Duration,
) -> Vec<Spectra6Color> {
    let mut canvas = Canvas::new(width as u32, height as u32);
    let style = MonoTextStyle::new(&FONT_10X20, Spectra6Color::Black);

    let full = build_text(message, retry_in);
    let bounds = Rectangle::new(
        Point::new(MARGIN_PX as i32, MARGIN_PX as i32),
        Size::new(
            (width as u32).saturating_sub(2 * MARGIN_PX),
            (height as u32).saturating_sub(2 * MARGIN_PX),
        ),
    );
    let _ = TextBox::new(&full, bounds, style).draw(&mut canvas);
    canvas.into_vec()
}

fn build_text(message: &str, retry_in: Duration) -> String {
    let mut s = String::from(message);
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s.push('\n');
    s.push_str(&format_retry(retry_in));
    s.push('\n');
    s.push('\n');
    s.push_str(RECONFIGURE_HINT);
    s
}

fn format_retry(d: Duration) -> String {
    // Round-to-nearest at each unit boundary so 119 s reads as "2 minutes",
    // not "1 minute" (which truncation gives).
    let secs = d.as_secs();
    if secs < 60 {
        format!(
            "Will retry in {} second{}.",
            secs,
            if secs == 1 { "" } else { "s" }
        )
    } else if secs < 3600 {
        let m = (secs + 30) / 60;
        format!(
            "Will retry in {} minute{}.",
            m,
            if m == 1 { "" } else { "s" }
        )
    } else {
        let h = (secs + 1800) / 3600;
        format!(
            "Will retry in {} hour{}.",
            h,
            if h == 1 { "" } else { "s" }
        )
    }
}
