//! An in-memory [`DrawTarget`] owning a row-major buffer of any
//! [`PixelColor`]. Used for compositing multiple elements (e.g. a QR code
//! plus text beneath it) into a single frame before handing that frame
//! off to the panel driver.
//!
//! Canvas itself is colour-space-agnostic — it doesn't know what "white"
//! means, the caller passes in the fill colour. Per-panel concepts like
//! `BLACK` / `WHITE` live on [`crate::panel::PanelColor`] and stay at
//! the call site.

use alloc::vec::Vec;
use core::convert::Infallible;
use embedded_graphics::pixelcolor::PixelColor;
use embedded_graphics::prelude::{DrawTarget, OriginDimensions, Pixel, Size};

/// A drawable rectangular region owning its pixel buffer.
pub struct Canvas<C> {
    pixels: Vec<C>,
    width: u32,
    height: u32,
}

impl<C: PixelColor> Canvas<C> {
    /// Allocate a `width × height` canvas filled with `fill`.
    pub fn new(width: u32, height: u32, fill: C) -> Self {
        let pixels = alloc::vec![fill; (width as usize) * (height as usize)];
        Self {
            pixels,
            width,
            height,
        }
    }

    /// Consume the canvas and return its underlying row-major pixel
    /// buffer.
    pub fn into_vec(self) -> Vec<C> {
        self.pixels
    }
}

impl<C> OriginDimensions for Canvas<C> {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

impl<C: PixelColor> DrawTarget for Canvas<C> {
    type Color = C;
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
