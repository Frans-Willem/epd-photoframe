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

use embedded_graphics::pixelcolor::{PixelColor, Rgb888};
use embedded_hal_async::spi::SpiBus;

pub trait PanelColor: PixelColor {
    const BLACK: Self;
    const WHITE: Self;

    /// Every paint colour the panel can render. Excludes any non-paint
    /// sentinel variants (e.g. Spectra-6's `Clean`) — callers may treat
    /// the iterated values as a complete palette.
    fn all() -> impl Iterator<Item = Self>;

    /// Quantise an arbitrary RGB to the closest panel colour. Each
    /// implementor picks its own metric — Spectra-6 uses a chromaticity
    /// closest-match, `Gray2` uses luma quantisation.
    fn from_rgb(rgb: Rgb888) -> Self;
}

#[allow(async_fn_in_trait)]
pub trait Panel<SPI: SpiBus> {
    type Color: PanelColor;
    type Error: core::fmt::Debug;

    /// Init-time mode picker. `()` for panels that have only one mode;
    /// drivers with multiple init sequences (e.g. UC8179's B/W vs
    /// 4-level grayscale) expose an enum and select via
    /// [`init_mode_for_palette`](Self::init_mode_for_palette). The
    /// `Default` impl is the safe choice when the colours aren't known
    /// yet (e.g. the all-white pre-flash).
    type InitMode: Default + Copy;

    const WIDTH: usize;
    const HEIGHT: usize;

    /// Map a flat output index in `0..WIDTH*HEIGHT` to the `(x, y)`
    /// coordinate the pixel should be fetched from in a row-major source
    /// image. Drivers that split the panel across multiple controllers
    /// (e.g. T133A01's left/right halves) override this to drive their
    /// stream order.
    fn output_index_to_image_xy(idx: usize) -> (usize, usize);

    /// Pick the init mode best-suited to the colours actually present
    /// in `palette`. Single-mode panels return `()` without walking the
    /// iterator; multi-mode panels iterate (with early exit where
    /// possible) to choose the cheapest waveform that covers the
    /// content.
    fn init_mode_for_palette(palette: impl IntoIterator<Item = Self::Color>) -> Self::InitMode;

    async fn enable(&mut self) -> Result<(), Self::Error>;
    async fn disable(&mut self) -> Result<(), Self::Error>;

    async fn reset(&mut self) -> Result<(), Self::Error>;
    async fn init(&mut self, spi: &mut SPI, mode: Self::InitMode) -> Result<(), Self::Error>;

    async fn power_on(&mut self, spi: &mut SPI) -> Result<(), Self::Error>;
    async fn power_off(&mut self, spi: &mut SPI) -> Result<(), Self::Error>;

    /// Stream `pixels` to the panel's frame buffer over SPI. The iterator
    /// must be `Clone`-able so multi-pass formats (e.g. UC8179's two
    /// 1bpp bit-planes for 4-level grayscale) can re-walk the same source
    /// without buffering the whole frame.
    async fn update_frame(
        &mut self,
        spi: &mut SPI,
        pixels: impl IntoIterator<Item = Self::Color> + Clone,
    ) -> Result<(), Self::Error>;

    /// Trigger a refresh and return immediately. Pair with
    /// [`wait_until_idle`](Self::wait_until_idle) to block on completion,
    /// or [`reset`](Self::reset) to abort.
    async fn display_frame_no_wait(&mut self, spi: &mut SPI) -> Result<(), Self::Error>;
    async fn wait_until_idle(&mut self) -> Result<(), Self::Error>;
}
