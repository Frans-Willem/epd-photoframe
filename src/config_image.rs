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
use embedded_text::TextBox;

use crate::canvas::Canvas;
use crate::qr_image;
use crate::spectra6::Spectra6Color;

/// Padding (panel pixels) on the outer panel edge and between the QR
/// and text regions of the config screen.
const LAYOUT_PADDING_PX: u32 = 24;

/// Render a configuration screen: `qr_payload` as a QR code on one side,
/// `instructions` (plain text; word-wrapped to the text area's width) on
/// the other. Returns the frame as a row-major `Vec<Spectra6Color>`.
pub fn render(
    width: usize,
    height: usize,
    qr_payload: &str,
    instructions: &str,
) -> Vec<Spectra6Color> {
    let mut canvas = Canvas::new(width as u32, height as u32);

    let (qr_area, text_area) = split_regions(width as u32, height as u32, LAYOUT_PADDING_PX);

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
        let bounds = Rectangle::new(Point::zero(), text_area.size);
        let _ = TextBox::new(instructions, bounds, style).draw(&mut sub);
    }

    canvas.into_vec()
}

/// Split a `width` x `height` rectangle into two sub-rectangles with
/// `padding` on every outer edge and once between them. The first
/// returned rectangle is a square sized to the shorter padded axis
/// (i.e. as large as possible while staying square); the second is
/// the remaining strip alongside it. The split goes along the longer
/// axis: left + right for landscape, top + bottom for portrait.
fn split_regions(width: u32, height: u32, padding: u32) -> (Rectangle, Rectangle) {
    if width >= height {
        // Landscape: square on the left, strip on the right.
        let square_side = height.saturating_sub(2 * padding);
        let strip_x = padding + square_side + padding;
        let strip_w = width.saturating_sub(strip_x + padding);
        (
            Rectangle::new(
                Point::new(padding as i32, padding as i32),
                Size::new(square_side, square_side),
            ),
            Rectangle::new(
                Point::new(strip_x as i32, padding as i32),
                Size::new(strip_w, height.saturating_sub(2 * padding)),
            ),
        )
    } else {
        // Portrait: square on top, strip below.
        let square_side = width.saturating_sub(2 * padding);
        let strip_y = padding + square_side + padding;
        let strip_h = height.saturating_sub(strip_y + padding);
        (
            Rectangle::new(
                Point::new(padding as i32, padding as i32),
                Size::new(square_side, square_side),
            ),
            Rectangle::new(
                Point::new(padding as i32, strip_y as i32),
                Size::new(width.saturating_sub(2 * padding), strip_h),
            ),
        )
    }
}
