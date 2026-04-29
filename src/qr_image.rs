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

/// Encode `payload` as a QR code and draw it onto `target`. The QR is
/// anchored at the top-left of `target.bounding_box()` and scaled to
/// the largest integer pixel-per-module ratio that fits. Each module
/// is drawn as a solid square filled with `dark` ("on" modules) or
/// `light` ("off" modules).
///
/// The caller is responsible for sizing the target so the desired
/// padding around the QR is already accounted for; this routine just
/// fills the box from the top-left.
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
    let qr_size = qr.size() as u32;
    let scale = bbox.size.width.min(bbox.size.height) / qr_size;
    if scale == 0 {
        return Ok(());
    }
    let origin_x = bbox.top_left.x;
    let origin_y = bbox.top_left.y;

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
