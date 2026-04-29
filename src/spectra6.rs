use crate::panel::PanelColor;
use embedded_graphics::pixelcolor::raw::RawU4;
use embedded_graphics::pixelcolor::{PixelColor, Rgb888, RgbColor};

#[derive(Clone, Copy, Eq, PartialEq, Default)]
pub enum Spectra6Color {
    Black = 0,
    #[default]
    White = 1,
    Yellow = 2,
    Red = 3,
    Blue = 5,
    Green = 6,
    Clean = 7,
}

impl PixelColor for Spectra6Color {
    type Raw = RawU4;
}

impl PanelColor for Spectra6Color {
    const BLACK: Self = Spectra6Color::Black;
    const WHITE: Self = Spectra6Color::White;

    fn all() -> impl Iterator<Item = Self> {
        [
            Spectra6Color::Black,
            Spectra6Color::White,
            Spectra6Color::Yellow,
            Spectra6Color::Red,
            Spectra6Color::Blue,
            Spectra6Color::Green,
            Spectra6Color::Clean,
        ]
        .into_iter()
    }

    fn to_rgb(&self) -> Option<Rgb888> {
        SPECTRA_6_PALETTE
            .iter()
            .find_map(|(rgb, c)| (*c == *self).then_some(*rgb))
    }

    /// Hand-tuned decision tree, faster than the default closest-match
    /// search and correct for the panel's six paint colours.
    fn from_rgb(value: Rgb888) -> Self {
        if value.r() < 105 {
            if value.b() < 109 {
                if value.g() < 62 {
                    Spectra6Color::Black
                } else {
                    Spectra6Color::Green
                }
            } else {
                Spectra6Color::Blue
            }
        } else if value.g() < 120 {
            Spectra6Color::Red
        } else if value.b() < 150 {
            Spectra6Color::Yellow
        } else {
            Spectra6Color::White
        }
    }
}

pub struct SpectraPacker<T>(pub T);

impl<T> Iterator for SpectraPacker<T>
where
    T: Iterator<Item = Spectra6Color>,
{
    type Item = u8;
    fn next(&mut self) -> Option<Self::Item> {
        let left = self.0.next()?;
        let right = self.0.next().unwrap_or(Spectra6Color::White);
        Some((left as u8) << 4 | (right as u8))
    }
}

/* Quick test pattern for Spectra 6 display */
#[allow(dead_code)]
pub fn test_screen(width: usize, height: usize) -> impl Iterator<Item = Spectra6Color> {
    (0..width * height).map(move |index| {
        let x = index % width;
        let y = index / width;
        match ((x / 32) + (y / 32)) % 6 {
            0 => Spectra6Color::White,
            1 => Spectra6Color::Black,
            2 => Spectra6Color::Red,
            3 => Spectra6Color::Green,
            4 => Spectra6Color::Blue,
            5 => Spectra6Color::Yellow,
            _ => Spectra6Color::White,
        }
    })
}

pub const SPECTRA_6_PALETTE: &[(Rgb888, Spectra6Color)] = &[
    (Rgb888::new(0x19, 0x1E, 0x21), Spectra6Color::Black),
    (Rgb888::new(0xE8, 0xE8, 0xE8), Spectra6Color::White),
    (Rgb888::new(0x21, 0x57, 0xBA), Spectra6Color::Blue),
    (Rgb888::new(0x12, 0x5F, 0x20), Spectra6Color::Green),
    (Rgb888::new(0xB2, 0x13, 0x18), Spectra6Color::Red),
    (Rgb888::new(0xEF, 0xDE, 0x44), Spectra6Color::Yellow),
];

pub const SPECTRA_6_PALETTE_SATURATED: &[(Rgb888, Spectra6Color)] = &[
    (Rgb888::new(0, 0, 0), Spectra6Color::Black),
    (Rgb888::new(255, 255, 255), Spectra6Color::White),
    (Rgb888::new(33, 87, 186), Spectra6Color::Blue),
    (Rgb888::new(18, 95, 32), Spectra6Color::Green),
    (Rgb888::new(178, 19, 24), Spectra6Color::Red),
    (Rgb888::new(239, 222, 68), Spectra6Color::Yellow),
];
