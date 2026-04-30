use crate::iter_util::ChunksHeaplessExt;
use crate::panel::Panel;
use core::marker::PhantomData;
use embassy_time::{Duration, Timer};
use embedded_graphics::pixelcolor::{Gray2, GrayColor};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiBus;

/// Pack one bit-plane of UC8179's 4-gray frame format. UC8179 ingests
/// 4-level grayscale as two 1bpp planes uploaded sequentially (cmd
/// `0x10` for the high bit, `0x13` for the low bit of each pixel's
/// 2-bit grey code). `SHIFT = 1` picks the high bit, `SHIFT = 0` the
/// low. Eight source pixels pack MSB-first into one output byte.
///
/// A trailing partial chunk (fewer than 8 pixels) is padded with
/// white bits (`1`) so truncated input doesn't leave dirty pixels.
fn pack_plane<I, const SHIFT: u8>(iter: I) -> impl Iterator<Item = u8>
where
    I: Iterator<Item = Gray2>,
{
    iter.chunks_heapless::<8>().map(|chunk| {
        let n = chunk.len() as u8;
        let byte = chunk
            .into_iter()
            .fold(0u8, |acc, g| (acc << 1) | ((g.luma() >> SHIFT) & 1));
        if n < 8 {
            (byte << (8 - n)) | ((1u8 << (8 - n)) - 1)
        } else {
            byte
        }
    })
}

// Panel: GooDisplay GDEY075T7 (800x480, B/W with 4-level grayscale, found in
// reTerminal E1001). Controller: Ultrachip UC8179.

#[allow(non_camel_case_types, dead_code)]
#[derive(Copy, Clone)]
enum Command {
    PanelSetting = 0x00,           // PSR
    PowerSetting = 0x01,           // PWR
    PowerOff = 0x02,               // POF
    PowerOn = 0x04,                // PON
    DataStartTransmission1 = 0x10, // DTM1 — 4-gray plane "high" bit
    DisplayRefresh = 0x12,         // DRF
    DataStartTransmission2 = 0x13, // DTM2 — 4-gray plane "low" bit
    DualSpi = 0x15,                // DSPI / SPI mode (single = 0x00)
    Lut20Vcom = 0x20,
    Lut21Ww = 0x21,
    Lut22Bw = 0x22,
    Lut23Wb = 0x23,
    Lut24Bb = 0x24,
    Lut25Bd = 0x25,
    Cdi = 0x50, // VCOM and data-interval setting
    Tcon = 0x60,
    Tres = 0x61, // resolution
}

impl From<Command> for u8 {
    fn from(c: Command) -> u8 {
        c as u8
    }
}

// 4-level grayscale waveform LUTs lifted verbatim from GxEPD2_4G v1.0.9
// (`src/epd/GxEPD2_750_T7.cpp`). Each LUT is 42 bytes (7 groups × 6
// bytes); LUT-BD has only 6 meaningful bytes upstream and is
// zero-padded to 42.
//
// Single-source. The UC8179 datasheet documents LUTC (0x20), LUTKW
// (0x22), LUTWK (0x23), and LUTKK (0x24) as 60-byte LUTs (10 × 6) —
// i.e. 18 bytes longer than what we send. GxEPD2_4G's short LUTs are
// production-validated, so the chip is presumably zero-padding the
// missing groups (0-frame phases = no-op). Good Display's official
// GDEY075T7 sample is 1bpp B&W only and doesn't ship a 4-gray LUT
// reference. If hardware testing shows artefacts, the first thing to
// try is padding the four 60-byte LUTs to their documented length with
// trailing zeros.
const LUT_VCOM_4G: [u8; 42] = [
    0x00, 0x0A, 0x00, 0x00, 0x00, 0x01, 0x60, 0x14, 0x14, 0x00, 0x00, 0x01, 0x00, 0x14, 0x00, 0x00,
    0x00, 0x01, 0x00, 0x13, 0x0A, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const LUT_WW_4G: [u8; 42] = [
    0x40, 0x0A, 0x00, 0x00, 0x00, 0x01, 0x90, 0x14, 0x14, 0x00, 0x00, 0x01, 0x10, 0x14, 0x0A, 0x00,
    0x00, 0x01, 0xA0, 0x13, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const LUT_BW_4G: [u8; 42] = [
    0x40, 0x0A, 0x00, 0x00, 0x00, 0x01, 0x90, 0x14, 0x14, 0x00, 0x00, 0x01, 0x00, 0x14, 0x0A, 0x00,
    0x00, 0x01, 0x99, 0x0C, 0x01, 0x03, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const LUT_WB_4G: [u8; 42] = [
    0x40, 0x0A, 0x00, 0x00, 0x00, 0x01, 0x90, 0x14, 0x14, 0x00, 0x00, 0x01, 0x00, 0x14, 0x0A, 0x00,
    0x00, 0x01, 0x99, 0x0B, 0x04, 0x04, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const LUT_BB_4G: [u8; 42] = [
    0x80, 0x0A, 0x00, 0x00, 0x00, 0x01, 0x90, 0x14, 0x14, 0x00, 0x00, 0x01, 0x20, 0x14, 0x0A, 0x00,
    0x00, 0x01, 0x50, 0x13, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const LUT_BD_4G: [u8; 42] = [
    0x00, 0x1E, 0x05, 0x1E, 0x05, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

pub enum Gdey075t7Error<SPI, CS, BUSY, DC, RST>
where
    SPI: SpiBus,
    CS: OutputPin,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
{
    SPIError(SPI::Error),
    CSError(CS::Error),
    BUSYError(BUSY::Error),
    DCError(DC::Error),
    RSTError(RST::Error),
}

impl<SPI, CS, BUSY, DC, RST> core::fmt::Debug for Gdey075t7Error<SPI, CS, BUSY, DC, RST>
where
    SPI: SpiBus,
    CS: OutputPin,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SPIError(x) => write!(f, "SPIError({:?})", x),
            Self::CSError(x) => write!(f, "CSError({:?})", x),
            Self::BUSYError(x) => write!(f, "BUSYError({:?})", x),
            Self::DCError(x) => write!(f, "DCError({:?})", x),
            Self::RSTError(x) => write!(f, "RSTError({:?})", x),
        }
    }
}

pub struct Gdey075t7<SPI, CS, BUSY, DC, RST> {
    _spi: PhantomData<SPI>,
    cs: CS,
    busy: BUSY,
    dc: DC,
    rst: RST,
}

impl<SPI, CS, BUSY, DC, RST> Gdey075t7<SPI, CS, BUSY, DC, RST>
where
    CS: OutputPin,
{
    /// `_spi` is taken only to fix `SPI` at the call site without
    /// requiring a turbofish; the bus itself isn't stored.
    pub fn new(_spi: &mut SPI, cs: CS, busy: BUSY, dc: DC, rst: RST) -> Self {
        let mut cs = cs;
        cs.set_high().unwrap();
        Gdey075t7 {
            _spi: PhantomData,
            cs,
            busy,
            dc,
            rst,
        }
    }
}

impl<SPI, CS, BUSY, DC, RST> Gdey075t7<SPI, CS, BUSY, DC, RST>
where
    SPI: SpiBus,
    CS: OutputPin,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
{
    async fn command(
        &mut self,
        spi: &mut SPI,
        command: Command,
        data: impl IntoIterator<Item = u8>,
    ) -> Result<(), Gdey075t7Error<SPI, CS, BUSY, DC, RST>> {
        self.cs.set_low().map_err(Gdey075t7Error::CSError)?;
        self.dc.set_low().map_err(Gdey075t7Error::DCError)?;
        spi.write(&[command.into()])
            .await
            .map_err(Gdey075t7Error::SPIError)?;
        self.dc.set_high().map_err(Gdey075t7Error::DCError)?;
        for chunk in data.into_iter().chunks_heapless::<128>() {
            spi.write(&chunk).await.map_err(Gdey075t7Error::SPIError)?;
        }
        self.cs.set_high().map_err(Gdey075t7Error::CSError)?;
        Ok(())
    }
}

impl<SPI, CS, BUSY, DC, RST> Panel<SPI> for Gdey075t7<SPI, CS, BUSY, DC, RST>
where
    SPI: SpiBus,
    CS: OutputPin,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
{
    type Color = Gray2;
    type Error = Gdey075t7Error<SPI, CS, BUSY, DC, RST>;

    const WIDTH: usize = 800;
    const HEIGHT: usize = 480;

    fn output_index_to_image_xy(idx: usize) -> (usize, usize) {
        (idx % Self::WIDTH, idx / Self::WIDTH)
    }

    async fn enable(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn disable(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn reset(&mut self) -> Result<(), Self::Error> {
        self.rst.set_high().map_err(Gdey075t7Error::RSTError)?;
        Timer::after(Duration::from_millis(10)).await;
        self.rst.set_low().map_err(Gdey075t7Error::RSTError)?;
        Timer::after(Duration::from_millis(10)).await;
        self.rst.set_high().map_err(Gdey075t7Error::RSTError)?;
        Timer::after(Duration::from_millis(10)).await;
        Ok(())
    }

    async fn init(&mut self, spi: &mut SPI) -> Result<(), Self::Error> {
        // Init sequence is GxEPD2's known-good 1bpp init with the 4-gray
        // overrides from GxEPD2_4G — panel-setting bit 0x20 selects "LUT
        // from registers" (so cmds 0x20..0x25 below take effect), and
        // VCOM-and-data-interval changes from the 1bpp `0x29, 0x07` to
        // `0x31, 0x07` for 4-gray timing.
        self.command(spi, Command::PowerSetting, [0x07, 0x07, 0x3F, 0x3F])
            .await?;
        self.command(spi, Command::PanelSetting, [0x3F]).await?;
        self.command(spi, Command::Tres, [0x03, 0x20, 0x01, 0xE0])
            .await?;
        self.command(spi, Command::DualSpi, [0x00]).await?;
        self.command(spi, Command::Cdi, [0x31, 0x07]).await?;
        self.command(spi, Command::Tcon, [0x22]).await?;

        // 4-gray waveform LUTs.
        self.command(spi, Command::Lut20Vcom, LUT_VCOM_4G).await?;
        self.command(spi, Command::Lut21Ww, LUT_WW_4G).await?;
        self.command(spi, Command::Lut22Bw, LUT_BW_4G).await?;
        self.command(spi, Command::Lut23Wb, LUT_WB_4G).await?;
        self.command(spi, Command::Lut24Bb, LUT_BB_4G).await?;
        self.command(spi, Command::Lut25Bd, LUT_BD_4G).await?;
        Ok(())
    }

    async fn power_on(&mut self, spi: &mut SPI) -> Result<(), Self::Error> {
        self.command(spi, Command::PowerOn, []).await?;
        self.wait_until_idle().await?;
        Ok(())
    }

    async fn power_off(&mut self, spi: &mut SPI) -> Result<(), Self::Error> {
        self.command(spi, Command::PowerOff, []).await?;
        self.wait_until_idle().await?;
        Ok(())
    }

    /// Stream the frame as two 1bpp bit-planes — DTM1 (`0x10`) carries
    /// the high bit of each pixel's 2-bit grey code, DTM2 (`0x13`) the
    /// low bit. The iterator is walked twice (once per plane); no
    /// intermediate frame buffer is allocated.
    async fn update_frame(
        &mut self,
        spi: &mut SPI,
        pixels: impl IntoIterator<Item = Self::Color> + Clone,
    ) -> Result<(), Self::Error> {
        self.command(
            spi,
            Command::DataStartTransmission1,
            pack_plane::<_, 1>(pixels.clone().into_iter()),
        )
        .await?;
        self.command(
            spi,
            Command::DataStartTransmission2,
            pack_plane::<_, 0>(pixels.into_iter()),
        )
        .await?;
        Ok(())
    }

    async fn display_frame_no_wait(&mut self, spi: &mut SPI) -> Result<(), Self::Error> {
        self.command(spi, Command::DisplayRefresh, []).await
    }

    async fn wait_until_idle(&mut self) -> Result<(), Self::Error> {
        // UC8179 idles BUSY high, busy low — same direction as GDEP073E01.
        self.busy
            .wait_for_high()
            .await
            .map_err(Gdey075t7Error::BUSYError)
    }
}
