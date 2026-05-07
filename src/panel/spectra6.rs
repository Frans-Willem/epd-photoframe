use super::PanelColor;
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
    /// Panel-internal "clean" / discharge level. Not a paint colour;
    /// excluded from [`PanelColor::all`].
    #[allow(dead_code)]
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
        ]
        .into_iter()
    }

    /// Closest-match search against [`SPECTRA_6_CHROMA_ANCHORS`] in
    /// `ChromaColor` space. Falls back to `Spectra6Color::default()`
    /// (`White`) only if the anchor table is empty — unreachable in
    /// practice, but cheaper than an `.unwrap()` panic path.
    fn from_rgb(value: Rgb888) -> Self {
        let pt = ChromaColor::from_rgb(value);
        SPECTRA_6_CHROMA_ANCHORS
            .iter()
            .min_by_key(|(c, _)| c.dist_sq(&pt))
            .map(|(_, color)| *color)
            .unwrap_or_default()
    }
}

/// A point in an integer Cartesian-chromaticity colour space derived from
/// sRGB. Each component is an `i16`, all three share the same 0..510
/// magnitude range, and Euclidean distance is balanced across hue, chroma
/// magnitude, and lightness — so closest-match against a fixed anchor set
/// is just `min_by_key(|a| a.dist_sq(&pt))`.
///
/// ```text
/// x = 2R − G − B          ∈ [-510, 510]   chroma along R ↔ CMY
/// y = 2·(G − B)           ∈ [-510, 510]   chroma along G ↔ M
/// v = 2·max(R, G, B)      ∈ [   0, 510]   value (HSV's V, scaled ×2)
/// ```
///
/// Topologically equivalent to the `(S·cos H, S·sin H, V)` cylindrical
/// projection of HSV but with HSV's hexagonal hue replaced by Cartesian
/// chromaticity. The `√3/2` factor that would round the chroma plane to a
/// perfect circle is approximated as 1, leaving a slight ellipse — fine
/// for closest-match classification because both anchors and inputs live
/// in the same space. Pure integer arithmetic; no division, no trig.
#[derive(Clone, Copy)]
struct ChromaColor {
    x: i16,
    y: i16,
    v: i16,
}

impl ChromaColor {
    fn from_rgb(rgb: Rgb888) -> Self {
        let r = rgb.r() as i16;
        let g = rgb.g() as i16;
        let b = rgb.b() as i16;
        ChromaColor {
            x: 2 * r - g - b,
            y: 2 * (g - b),
            v: 2 * r.max(g).max(b),
        }
    }

    fn dist_sq(&self, other: &Self) -> i32 {
        let dx = (self.x - other.x) as i32;
        let dy = (self.y - other.y) as i32;
        let dv = (self.v - other.v) as i32;
        dx * dx + dy * dy + dv * dv
    }
}

/// Mean chromaticity of each Spectra-6 colour across the 16 sample palettes
/// the firmware expects to receive — all from the `epd-dither` crate:
///
/// - 14 panel calibration variants in `epd_dither::spectra6`
///   (`SPECTRA6_D{50,65}{,_ADJUSTED,_BPC{50,75,80,90,100}_ADJUSTED}`)
/// - `epd_dither::decompose::naive::EPDOPTIMIZE` (taken as-is from the
///   `epdoptimize` toolchain)
/// - `epd_dither::decompose::octahedron::NAIVE_RGB6` (pure primaries and
///   secondaries — the untuned octahedral reference)
///
/// Means computed in `ChromaColor` space and rounded to the nearest
/// integer; the resulting closest-anchor classifier agrees with all
/// 96 (palette × colour) reference rows.
const SPECTRA_6_CHROMA_ANCHORS: &[(ChromaColor, Spectra6Color)] = &[
    (
        ChromaColor {
            x: -30,
            y: -36,
            v: 93,
        },
        Spectra6Color::Black,
    ),
    (
        ChromaColor {
            x: -42,
            y: -13,
            v: 451,
        },
        Spectra6Color::White,
    ),
    (
        ChromaColor {
            x: 239,
            y: 402,
            v: 450,
        },
        Spectra6Color::Yellow,
    ),
    (
        ChromaColor {
            x: 310,
            y: 16,
            v: 328,
        },
        Spectra6Color::Red,
    ),
    (
        ChromaColor {
            x: -268,
            y: -195,
            v: 369,
        },
        Spectra6Color::Blue,
    ),
    (
        ChromaColor {
            x: -100,
            y: 121,
            v: 301,
        },
        Spectra6Color::Green,
    ),
];

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
