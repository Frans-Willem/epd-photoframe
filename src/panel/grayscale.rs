//! `PanelColor` impl for embedded-graphics' built-in `Gray2` (2 bpp,
//! four levels). Used by the GDEY075T7 panel on the E1001.
//!
//! Levels map evenly to grayscale RGB at 0 / 85 / 170 / 255. Per-panel
//! midtone calibration (the panel's actual gray levels are not perfectly
//! linear) is a follow-up; see TODO.md.

use embedded_graphics::pixelcolor::{Gray2, GrayColor, Rgb888, RgbColor};

use super::PanelColor;

impl PanelColor for Gray2 {
    const BLACK: Self = <Self as GrayColor>::BLACK;
    const WHITE: Self = <Self as GrayColor>::WHITE;

    fn all() -> impl Iterator<Item = Self> {
        [Gray2::new(0), Gray2::new(1), Gray2::new(2), Gray2::new(3)].into_iter()
    }

    /// BT.601 luma + round-to-nearest of the four 85-step levels (centres
    /// at 0 / 85 / 170 / 255). Pure integer arithmetic.
    fn from_rgb(rgb: Rgb888) -> Self {
        let r = rgb.r() as u32;
        let g = rgb.g() as u32;
        let b = rgb.b() as u32;
        let luma = (299 * r + 587 * g + 114 * b + 500) / 1000;
        Gray2::new(((luma + 42) / 85).min(3) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(r: u8, g: u8, b: u8) -> u8 {
        Gray2::from_rgb(Rgb888::new(r, g, b)).luma()
    }

    #[test]
    fn anchor_grays_round_to_their_own_level() {
        assert_eq!(level(  0,   0,   0), 0);
        assert_eq!(level( 85,  85,  85), 1);
        assert_eq!(level(170, 170, 170), 2);
        assert_eq!(level(255, 255, 255), 3);
    }

    #[test]
    fn round_to_nearest_at_each_boundary() {
        // Boundaries fall at the midpoints 42.5 / 127.5 / 212.5; integer
        // round-half-up via `+42` puts the integer just below each midpoint
        // in the lower bucket and the one just above into the upper.
        assert_eq!(level( 42,  42,  42), 0);
        assert_eq!(level( 43,  43,  43), 1);
        assert_eq!(level(127, 127, 127), 1);
        assert_eq!(level(128, 128, 128), 2);
        assert_eq!(level(212, 212, 212), 2);
        assert_eq!(level(213, 213, 213), 3);
    }

    #[test]
    fn bt601_weighting() {
        // Pure red: luma = 0.299·255 ≈ 76 → bucket 1 (closer to 85 than 0).
        assert_eq!(level(255,   0,   0), 1);
        // Pure green: luma = 0.587·255 ≈ 150 → bucket 2 (closer to 170).
        assert_eq!(level(  0, 255,   0), 2);
        // Pure blue: luma = 0.114·255 ≈ 29 → bucket 0 (closer to 0).
        assert_eq!(level(  0,   0, 255), 0);
    }
}
