//! Build the full-panel image shown while the device is in
//! configuration mode: a QR code plus instructional text next to it.
//! Layout picks itself based on the panel's aspect ratio — QR on the
//! left with text on the right for landscape panels, QR on top with
//! text below for portrait panels.

use alloc::vec::Vec;

use embedded_graphics::Drawable;
use embedded_graphics::draw_target::DrawTargetExt;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::prelude::{Point, Size};
use embedded_graphics::primitives::Rectangle;
use embedded_graphics::text::{Baseline, Text};

use crate::canvas::Canvas;
use crate::qr_image;
use crate::spectra6::Spectra6Color;

/// Render a configuration screen: `qr_payload` as a QR code on one side,
/// `instructions` (newline-separated lines of plain text) on the other.
/// Returns the frame as a row-major `Vec<Spectra6Color>`.
pub fn render(
    width: usize,
    height: usize,
    qr_payload: &str,
    instructions: &str,
) -> Vec<Spectra6Color> {
    let mut canvas = Canvas::new(width as u32, height as u32);

    let (qr_area, text_area) = split_regions(width as u32, height as u32);

    {
        let mut sub = canvas.cropped(&qr_area);
        let _ = qr_image::draw(
            &mut sub,
            qr_payload,
            Spectra6Color::Black,
            Spectra6Color::White,
        );
    }
    {
        let mut sub = canvas.cropped(&text_area);
        let style = MonoTextStyle::new(&FONT_10X20, Spectra6Color::Black);
        let _ = Text::with_baseline(
            instructions,
            Point::new(16, 16),
            style,
            Baseline::Top,
        )
        .draw(&mut sub);
    }

    canvas.into_vec()
}

/// Split the panel rectangle into `(qr_area, text_area)`. The QR area is
/// a square sized to the shorter axis; the text area fills the remaining
/// strip alongside it.
fn split_regions(width: u32, height: u32) -> (Rectangle, Rectangle) {
    if width >= height {
        // Landscape: QR on the left, text on the right.
        let qr_side = height;
        (
            Rectangle::new(Point::zero(), Size::new(qr_side, qr_side)),
            Rectangle::new(
                Point::new(qr_side as i32, 0),
                Size::new(width - qr_side, height),
            ),
        )
    } else {
        // Portrait: QR on top, text below.
        let qr_side = width;
        (
            Rectangle::new(Point::zero(), Size::new(qr_side, qr_side)),
            Rectangle::new(
                Point::new(0, qr_side as i32),
                Size::new(width, height - qr_side),
            ),
        )
    }
}
