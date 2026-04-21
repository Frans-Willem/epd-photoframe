//! Draw a QR code onto any [`DrawTarget`], centred within its bounding
//! box with a small margin. Caller supplies the two colours used for
//! "on" and "off" modules, so the routine is reusable across different
//! colour spaces (Spectra 6 for our eInk panels, but nothing about the
//! encoder cares).

use embedded_graphics::{
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

/// Margin (in target-pixel units) left around the QR module grid when we
/// centre it, so the code isn't right against the bezel and is easier
/// for a phone camera to frame.
const MARGIN_PX: u32 = 32;

/// Encode `payload` as a QR code and draw it onto `target`. The QR is
/// centred within `target.bounding_box()` and scaled to the largest
/// integer pixel-per-module ratio that still leaves a `MARGIN_PX` margin.
/// Each module is drawn as a solid square filled with `dark` ("on"
/// modules) or `light` ("off" modules).
///
/// Returns `Ok(())` and draws nothing if the payload is too long for a
/// `Version::MAX` QR code or the target is too small to fit one pixel per
/// module.
pub fn draw<D>(
    target: &mut D,
    payload: &str,
    dark: D::Color,
    light: D::Color,
) -> Result<(), D::Error>
where
    D: DrawTarget,
{
    let mut out_buf = [0u8; Version::MAX.buffer_len()];
    let mut tmp_buf = [0u8; Version::MAX.buffer_len()];

    let qr = match QrCode::encode_text(
        payload,
        &mut tmp_buf,
        &mut out_buf,
        QrCodeEcc::Medium,
        Version::MIN,
        Version::MAX,
        None,
        true,
    ) {
        Ok(qr) => qr,
        Err(_) => return Ok(()),
    };

    let bbox = target.bounding_box();
    let width = bbox.size.width;
    let height = bbox.size.height;
    let qr_size = qr.size() as u32;
    let usable = width.min(height).saturating_sub(MARGIN_PX * 2);
    let scale = usable / qr_size;
    if scale == 0 {
        return Ok(());
    }
    let drawn = qr_size * scale;
    let origin_x = bbox.top_left.x + ((width - drawn) / 2) as i32;
    let origin_y = bbox.top_left.y + ((height - drawn) / 2) as i32;

    for my in 0..qr.size() {
        for mx in 0..qr.size() {
            let colour = if qr.get_module(mx, my) { dark } else { light };
            let px0 = origin_x + mx * scale as i32;
            let py0 = origin_y + my * scale as i32;
            Rectangle::new(Point::new(px0, py0), Size::new(scale, scale))
                .into_styled(PrimitiveStyle::with_fill(colour))
                .draw(target)?;
        }
    }

    Ok(())
}
