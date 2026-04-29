use crate::iter_util::ChunksHeaplessExt;
use crate::panel::Panel;
use crate::spectra6::{Spectra6Color, SpectraPacker};
use core::marker::PhantomData;
use embassy_time::{Duration, Timer};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiBus;

// Panel: GooDisplay GDEP073E01 (800x480, Spectra 6, found in reTerminal E1002).
// Controller appears similar to UC8159 / SPD1656.

#[allow(non_camel_case_types, dead_code)]
#[derive(Copy, Clone)]
enum Command {
    PanelSetting = 0x00,
    PowerSetting = 0x01,
    PowerOff = 0x02,
    POFS = 0x03,
    PowerOn = 0x04,
    BoosterSoftStart1 = 0x05,
    BoosterSoftStart2 = 0x06,
    DeepSleep = 0x07,
    BoosterSoftStart3 = 0x08,
    DataStartTransmission = 0x10,
    DisplayRefresh = 0x12,
    PllControl = 0x30,
    CDI = 0x50,
    TCON = 0x60,
    TRES = 0x61,
    T_VDCS = 0x84,
    CMDH = 0xAA,
    PWS = 0xE3,
}

impl From<Command> for u8 {
    fn from(c: Command) -> u8 {
        c as u8
    }
}

pub enum Gdep073e01Error<SPI, CS, BUSY, DC, RST>
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

impl<SPI, CS, BUSY, DC, RST> core::fmt::Debug for Gdep073e01Error<SPI, CS, BUSY, DC, RST>
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

pub struct Gdep073e01<SPI, CS, BUSY, DC, RST> {
    _spi: PhantomData<SPI>,
    cs: CS,
    busy: BUSY,
    dc: DC,
    rst: RST,
}

impl<SPI, CS, BUSY, DC, RST> Gdep073e01<SPI, CS, BUSY, DC, RST>
where
    CS: OutputPin,
{
    /// `_spi` is taken only to fix `SPI` at the call site without
    /// requiring a turbofish; the bus itself isn't stored.
    pub fn new(_spi: &mut SPI, cs: CS, busy: BUSY, dc: DC, rst: RST) -> Self {
        let mut cs = cs;
        cs.set_high().unwrap();
        Gdep073e01 {
            _spi: PhantomData,
            cs,
            busy,
            dc,
            rst,
        }
    }
}

impl<SPI, CS, BUSY, DC, RST> Gdep073e01<SPI, CS, BUSY, DC, RST>
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
    ) -> Result<(), Gdep073e01Error<SPI, CS, BUSY, DC, RST>> {
        self.cs.set_low().map_err(Gdep073e01Error::CSError)?;
        self.dc.set_low().map_err(Gdep073e01Error::DCError)?;
        spi.write(&[command.into()])
            .await
            .map_err(Gdep073e01Error::SPIError)?;
        self.dc.set_high().map_err(Gdep073e01Error::DCError)?;
        for chunk in data.into_iter().chunks_heapless::<128>() {
            spi.write(&chunk).await.map_err(Gdep073e01Error::SPIError)?;
        }
        self.cs.set_high().map_err(Gdep073e01Error::CSError)?;
        Ok(())
    }
}

impl<SPI, CS, BUSY, DC, RST> Panel<SPI> for Gdep073e01<SPI, CS, BUSY, DC, RST>
where
    SPI: SpiBus,
    CS: OutputPin,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
{
    type Color = Spectra6Color;
    type Error = Gdep073e01Error<SPI, CS, BUSY, DC, RST>;

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
        self.rst.set_high().map_err(Gdep073e01Error::RSTError)?;
        Timer::after(Duration::from_millis(10)).await;
        self.rst.set_low().map_err(Gdep073e01Error::RSTError)?;
        Timer::after(Duration::from_millis(10)).await;
        self.rst.set_high().map_err(Gdep073e01Error::RSTError)?;
        Timer::after(Duration::from_millis(10)).await;
        Ok(())
    }

    async fn init(&mut self, spi: &mut SPI) -> Result<(), Self::Error> {
        // Call after reset.
        self.command(spi, Command::CMDH, [0x49, 0x55, 0x20, 0x08, 0x09, 0x18])
            .await?;
        self.command(spi, Command::PowerSetting, [0x3F]).await?;
        self.command(spi, Command::PanelSetting, [0x5F, 0x69])
            .await?;
        self.command(spi, Command::POFS, [0x00, 0x54, 0x00, 0x44])
            .await?;
        self.command(spi, Command::BoosterSoftStart1, [0x40, 0x1F, 0x1F, 0x2C])
            .await?;
        self.command(spi, Command::BoosterSoftStart2, [0x6F, 0x1F, 0x17, 0x49])
            .await?;
        self.command(spi, Command::BoosterSoftStart3, [0x6F, 0x1F, 0x1F, 0x22])
            .await?;
        self.command(spi, Command::PllControl, [0x03]).await?;
        self.command(spi, Command::CDI, [0x3F]).await?;
        self.command(spi, Command::TCON, [0x02, 0x00]).await?;
        self.command(spi, Command::TRES, [0x03, 0x20, 0x01, 0xE0])
            .await?;
        self.command(spi, Command::T_VDCS, [0x01]).await?;
        self.command(spi, Command::PWS, [0x2F]).await?;
        Ok(())
    }

    async fn power_on(&mut self, spi: &mut SPI) -> Result<(), Self::Error> {
        self.command(spi, Command::PowerOn, []).await?;
        self.wait_until_idle().await?;
        Ok(())
    }

    async fn power_off(&mut self, spi: &mut SPI) -> Result<(), Self::Error> {
        self.command(spi, Command::PowerOff, [0x00]).await?;
        self.wait_until_idle().await?;
        Ok(())
    }

    async fn update_frame(
        &mut self,
        spi: &mut SPI,
        pixels: impl IntoIterator<Item = Self::Color> + Clone,
    ) -> Result<(), Self::Error> {
        self.command(
            spi,
            Command::DataStartTransmission,
            SpectraPacker(pixels.into_iter()),
        )
        .await
    }

    async fn display_frame_no_wait(&mut self, spi: &mut SPI) -> Result<(), Self::Error> {
        self.command(spi, Command::DisplayRefresh, [0x00]).await
    }

    async fn wait_until_idle(&mut self) -> Result<(), Self::Error> {
        // BUSY is low while the panel is busy, high when idle.
        self.busy
            .wait_for_high()
            .await
            .map_err(Gdep073e01Error::BUSYError)
    }
}
