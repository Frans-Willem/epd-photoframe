//! Panel-driver abstraction.
//!
//! The canonical lifecycle for one refresh cycle is:
//!
//! ```text
//! enable                       (once at startup; no-op without a board rail)
//!   reset                      (RST pulse)
//!   wait_until_idle            (controller ready)
//!   init                       (register configuration over SPI)
//!   power_on                   (controller turns on panel high-voltage rails)
//!     update_frame             (stream pixel data over SPI)
//!     display_frame_no_wait    (trigger the refresh)
//!     wait_until_idle          (~20 s e-ink transition)
//!   power_off                  (controller drops panel rails)
//! disable                      (once at shutdown; no-op without a board rail)
//! ```
//!
//! `enable` / `disable` toggle a board-level enable pin (E1004's TFT_EN).
//! Drivers without one make these no-ops. `power_on` / `power_off` are
//! controller-level commands over SPI: the high-voltage rails need to be
//! on for both the SPI pixel upload and the actual refresh.

use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_hal_async::spi::SpiBus;

pub trait PanelColor: Copy + Default {
    const BLACK: Self;
    const WHITE: Self;

    /// Every variant the panel's controller can emit, including any
    /// non-paint sentinel values. Sentinels should report `to_rgb() == None`
    /// so they're skipped by the default `from_rgb` closest-match search.
    fn all() -> impl Iterator<Item = Self>;

    /// RGB rendering of a paint colour, or `None` for sentinel values
    /// (e.g. Spectra6's `Clean`).
    fn to_rgb(&self) -> Option<Rgb888>;

    /// Closest match for an arbitrary RGB. Default implementation searches
    /// `all()` by squared Euclidean distance, skipping any variant whose
    /// `to_rgb` is `None`. Override if a faster lookup is available
    /// (e.g. Spectra6's hand-tuned decision tree).
    fn from_rgb(rgb: Rgb888) -> Self {
        Self::all()
            .filter_map(|c| c.to_rgb().map(|p| (c, p)))
            .min_by_key(|(_, p)| {
                let dr = p.r() as i32 - rgb.r() as i32;
                let dg = p.g() as i32 - rgb.g() as i32;
                let db = p.b() as i32 - rgb.b() as i32;
                dr * dr + dg * dg + db * db
            })
            .map(|(c, _)| c)
            .unwrap_or(Self::BLACK)
    }
}

#[allow(async_fn_in_trait)]
pub trait Panel<SPI: SpiBus> {
    type Color: PanelColor;
    type Error: core::fmt::Debug;

    const WIDTH: usize;
    const HEIGHT: usize;

    /// Map a flat output index in `0..WIDTH*HEIGHT` to the `(x, y)`
    /// coordinate the pixel should be fetched from in a row-major source
    /// image. Drivers that split the panel across multiple controllers
    /// (e.g. T133A01's left/right halves) override this to drive their
    /// stream order.
    fn output_index_to_image_xy(idx: usize) -> (usize, usize);

    async fn enable(&mut self) -> Result<(), Self::Error>;
    async fn disable(&mut self) -> Result<(), Self::Error>;

    async fn reset(&mut self) -> Result<(), Self::Error>;
    async fn init(&mut self, spi: &mut SPI) -> Result<(), Self::Error>;

    async fn power_on(&mut self, spi: &mut SPI) -> Result<(), Self::Error>;
    async fn power_off(&mut self, spi: &mut SPI) -> Result<(), Self::Error>;

    async fn update_frame(
        &mut self,
        spi: &mut SPI,
        pixels: impl IntoIterator<Item = Self::Color>,
    ) -> Result<(), Self::Error>;

    /// Trigger a refresh and return immediately. Pair with
    /// [`wait_until_idle`](Self::wait_until_idle) to block on completion,
    /// or [`reset`](Self::reset) to abort.
    async fn display_frame_no_wait(&mut self, spi: &mut SPI) -> Result<(), Self::Error>;
    async fn wait_until_idle(&mut self) -> Result<(), Self::Error>;
}
