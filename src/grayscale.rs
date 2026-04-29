//! `PanelColor` impl for embedded-graphics' built-in `Gray2` (2 bpp,
//! four levels). Used by the GDEY075T7 panel on the E1001.
//!
//! Levels map evenly to grayscale RGB at 0 / 85 / 170 / 255. Per-panel
//! midtone calibration (the panel's actual gray levels are not perfectly
//! linear) is a follow-up; see TODO.md.

use embedded_graphics::pixelcolor::{Gray2, GrayColor, Rgb888};

use crate::panel::PanelColor;

impl PanelColor for Gray2 {
    const BLACK: Self = <Self as GrayColor>::BLACK;
    const WHITE: Self = <Self as GrayColor>::WHITE;

    fn all() -> impl Iterator<Item = Self> {
        [Gray2::new(0), Gray2::new(1), Gray2::new(2), Gray2::new(3)].into_iter()
    }

    fn to_rgb(&self) -> Option<Rgb888> {
        let g = self.luma() * 85;
        Some(Rgb888::new(g, g, g))
    }
}
