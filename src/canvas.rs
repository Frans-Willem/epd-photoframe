//! An in-memory [`DrawTarget`] owning a row-major [`Spectra6Color`]
//! buffer. Used for compositing multiple elements (e.g. a QR code plus
//! text beneath it) into a single frame before handing that frame off to
//! the panel driver.

use alloc::vec::Vec;
use core::convert::Infallible;
use embedded_graphics::prelude::{DrawTarget, OriginDimensions, Pixel, Size};

use crate::spectra6::Spectra6Color;

/// A drawable rectangular region owning its pixel buffer.
pub struct Canvas {
    pixels: Vec<Spectra6Color>,
    width: u32,
    height: u32,
}

impl Canvas {
    /// Allocate a `width × height` canvas filled with white.
    pub fn new(width: u32, height: u32) -> Self {
        let pixels =
            alloc::vec![Spectra6Color::White; (width as usize) * (height as usize)];
        Self {
            pixels,
            width,
            height,
        }
    }

    /// Consume the canvas and return its underlying row-major pixel
    /// buffer.
    pub fn into_vec(self) -> Vec<Spectra6Color> {
        self.pixels
    }
}

impl OriginDimensions for Canvas {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

impl DrawTarget for Canvas {
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
