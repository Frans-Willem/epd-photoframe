use super::Panel;
use super::spectra6::{Spectra6Color, SpectraPacker};
use crate::iter_util::ChunksHeaplessExt;
use core::marker::PhantomData;
use embassy_time::{Duration, Timer};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiBus;

const SINGLE_BYTE_WRITE: bool = false;
const PANEL_WIDTH: usize = 1200;
const PANEL_HEIGHT: usize = 1600;

#[allow(non_camel_case_types, dead_code)]
#[derive(Copy, Clone)]
enum Command {
    PanelSetting = 0x00, // PSR
    PowerSetting = 0x01, // PWR/PWRR
    PowerOff = 0x02,
    PowerOn = 0x04,
    BoosterSoftStart1 = 0x05,     // BTST_N
    BoosterSoftStart2 = 0x06,     // BTST_P
    DataStartTransmission = 0x10, // DTM
    DisplayRefresh = 0x12,        // DRF
    LUT0 = 0x20,
    PllControl = 0x30, // PLL
    TSC = 0x40,
    CDI = 0x50,
    Cmd60 = 0x60,
    TRES = 0x61,
    Cmd74 = 0x74,
    AMV = 0x80,
    VV = 0x81,
    VDCS = 0x82,
    Cmd86 = 0x86,
    PGM = 0x90,
    APG = 0x91,
    ROTP = 0x92,
    DCDC = 0xA5,
    CmdB0 = 0xB0,
    CmdB1 = 0xB1,
    CmdB6 = 0xB6,
    CmdB7 = 0xB7,
    CCSET = 0xE0,
    PWS = 0xE3,
    TSSSET = 0xE5,
    CmdF0 = 0xF0,
}

enum Controller {
    Master,
    Slave,
    Both,
}

impl From<Command> for u8 {
    fn from(c: Command) -> u8 {
        c as u8
    }
}

pub enum T133A01Error<SPI, CS_MASTER, CS_SLAVE, BUSY, DC, RST, EN>
where
    SPI: SpiBus,
    CS_MASTER: OutputPin,
    CS_SLAVE: OutputPin,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
    EN: OutputPin,
{
    SPIError(SPI::Error),
    CSMasterError(CS_MASTER::Error),
    CSSlaveError(CS_SLAVE::Error),
    BUSYError(BUSY::Error),
    DCError(DC::Error),
    RSTError(RST::Error),
    ENError(EN::Error),
}

impl<SPI, CS_MASTER, CS_SLAVE, BUSY, DC, RST, EN> core::fmt::Debug
    for T133A01Error<SPI, CS_MASTER, CS_SLAVE, BUSY, DC, RST, EN>
where
    SPI: SpiBus,
    CS_MASTER: OutputPin,
    CS_SLAVE: OutputPin,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
    EN: OutputPin,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SPIError(x) => write!(f, "SPIError({:?})", x),
            Self::CSMasterError(x) => write!(f, "CSMasterError({:?})", x),
            Self::CSSlaveError(x) => write!(f, "CSSlaveError({:?})", x),
            Self::BUSYError(x) => write!(f, "BUSYError({:?})", x),
            Self::DCError(x) => write!(f, "DCError({:?})", x),
            Self::RSTError(x) => write!(f, "RSTError({:?})", x),
            Self::ENError(x) => write!(f, "ENError({:?})", x),
        }
    }
}

pub struct T133A01<SPI, CS_MASTER, CS_SLAVE, BUSY, DC, RST, EN> {
    _spi: PhantomData<SPI>,
    cs_master: CS_MASTER,
    cs_slave: CS_SLAVE,
    busy: BUSY,
    dc: DC,
    rst: RST,
    en: EN,
}

impl<SPI, CS_MASTER, CS_SLAVE, BUSY, DC, RST, EN>
    T133A01<SPI, CS_MASTER, CS_SLAVE, BUSY, DC, RST, EN>
where
    CS_MASTER: OutputPin,
    CS_SLAVE: OutputPin,
{
    /// `_spi` is taken only to fix `SPI` at the call site without
    /// requiring a turbofish; the bus itself isn't stored.
    pub fn new(
        _spi: &mut SPI,
        cs_master: CS_MASTER,
        cs_slave: CS_SLAVE,
        busy: BUSY,
        dc: DC,
        rst: RST,
        en: EN,
    ) -> Self {
        let mut cs_master = cs_master;
        cs_master.set_high().unwrap();
        let mut cs_slave = cs_slave;
        cs_slave.set_high().unwrap();
        T133A01 {
            _spi: PhantomData,
            cs_master,
            cs_slave,
            busy,
            dc,
            rst,
            en,
        }
    }
}

impl<SPI, CS_MASTER, CS_SLAVE, BUSY, DC, RST, EN>
    T133A01<SPI, CS_MASTER, CS_SLAVE, BUSY, DC, RST, EN>
where
    SPI: SpiBus,
    CS_MASTER: OutputPin,
    CS_SLAVE: OutputPin,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
    EN: OutputPin,
{
    pub fn is_busy(&mut self) -> bool {
        self.busy.is_low().unwrap()
    }

    async fn command(
        &mut self,
        spi: &mut SPI,
        controller: Controller,
        command: Command,
        data: impl IntoIterator<Item = u8>,
    ) -> Result<(), T133A01Error<SPI, CS_MASTER, CS_SLAVE, BUSY, DC, RST, EN>> {
        // Assert chip select, pull low
        match controller {
            Controller::Master => self
                .cs_master
                .set_low()
                .map_err(T133A01Error::CSMasterError)?,
            Controller::Slave => self
                .cs_slave
                .set_low()
                .map_err(T133A01Error::CSSlaveError)?,
            Controller::Both => {
                self.cs_master
                    .set_low()
                    .map_err(T133A01Error::CSMasterError)?;
                self.cs_slave
                    .set_low()
                    .map_err(T133A01Error::CSSlaveError)?;
            }
        };
        // Write command
        self.dc.set_high().map_err(T133A01Error::DCError)?;
        spi.write(&[command.into()])
            .await
            .map_err(T133A01Error::SPIError)?;
        // Write data
        self.dc.set_low().map_err(T133A01Error::DCError)?;
        if SINGLE_BYTE_WRITE {
            for val in data.into_iter() {
                spi.write(&[val]).await.map_err(T133A01Error::SPIError)?;
            }
        } else {
            for chunk in data.into_iter().chunks_heapless::<128>() {
                spi.write(&chunk).await.map_err(T133A01Error::SPIError)?;
            }
        }
        // Deassert chip select, pull high
        match controller {
            Controller::Master => self
                .cs_master
                .set_high()
                .map_err(T133A01Error::CSMasterError)?,
            Controller::Slave => self
                .cs_slave
                .set_high()
                .map_err(T133A01Error::CSSlaveError)?,
            Controller::Both => {
                self.cs_master
                    .set_high()
                    .map_err(T133A01Error::CSMasterError)?;
                self.cs_slave
                    .set_high()
                    .map_err(T133A01Error::CSSlaveError)?;
            }
        };
        Ok(())
    }
}

impl<SPI, CS_MASTER, CS_SLAVE, BUSY, DC, RST, EN> Panel<SPI>
    for T133A01<SPI, CS_MASTER, CS_SLAVE, BUSY, DC, RST, EN>
where
    SPI: SpiBus,
    CS_MASTER: OutputPin,
    CS_SLAVE: OutputPin,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
    EN: OutputPin,
{
    type Color = Spectra6Color;
    type Error = T133A01Error<SPI, CS_MASTER, CS_SLAVE, BUSY, DC, RST, EN>;
    type InitMode = ();

    const WIDTH: usize = PANEL_WIDTH;
    const HEIGHT: usize = PANEL_HEIGHT;

    fn init_mode_for_palette(_palette: impl IntoIterator<Item = Self::Color>) -> Self::InitMode {}

    /// The T133A01 has master/slave controllers each taking a half-width
    /// stripe, so the iterator emits the left half fully before the right
    /// half.
    fn output_index_to_image_xy(idx: usize) -> (usize, usize) {
        let half_width = PANEL_WIDTH / 2;
        let x = idx % half_width;
        let y_total = idx / half_width;
        let half = y_total / PANEL_HEIGHT;
        let y = y_total % PANEL_HEIGHT;
        let x = if half > 0 { x + half_width } else { x };
        (x, y)
    }

    async fn enable(&mut self) -> Result<(), Self::Error> {
        self.en.set_high().map_err(T133A01Error::ENError)
    }

    async fn disable(&mut self) -> Result<(), Self::Error> {
        self.en.set_low().map_err(T133A01Error::ENError)
    }

    async fn reset(&mut self) -> Result<(), Self::Error> {
        // TODO: Can I lower these to 10ms?
        self.rst.set_high().map_err(T133A01Error::RSTError)?;
        Timer::after(Duration::from_millis(20)).await;
        self.rst.set_low().map_err(T133A01Error::RSTError)?;
        Timer::after(Duration::from_millis(20)).await;
        self.rst.set_high().map_err(T133A01Error::RSTError)?;
        Timer::after(Duration::from_millis(20)).await;
        Ok(())
    }

    async fn init(&mut self, spi: &mut SPI, _mode: Self::InitMode) -> Result<(), Self::Error> {
        // NOTE: Call after reset
        self.command(
            spi,
            Controller::Master,
            Command::Cmd74, // Could be VCOM?
            [0x00, 0x0C, 0x0C, 0xD9, 0xDD, 0xDD, 0x15, 0x15, 0x55],
            // ESPHome does [0xC0, 0x1C, 0x1C, 0xCC, 0xCC, 0xCC, 0x15, 0x15, 0x55]
        )
        .await?;
        self.command(
            spi,
            Controller::Both,
            Command::CmdF0,
            [0x49, 0x55, 0x13, 0x5D, 0x05, 0x10],
        )
        .await?;
        self.wait_until_idle().await?;
        self.command(spi, Controller::Both, Command::PanelSetting, [0xDF, 0x69])
            .await?;
        self.command(spi, Controller::Master, Command::DCDC, [0x44, 0x54, 0x00])
            .await?;
        self.command(spi, Controller::Both, Command::CDI, [0x37])
            .await?;
        self.command(spi, Controller::Both, Command::Cmd60, [0x03, 0x03])
            .await?;
        self.command(spi, Controller::Both, Command::Cmd86, [0x10])
            .await?;
        self.command(spi, Controller::Both, Command::PWS, [0x22])
            .await?;
        self.command(
            spi,
            Controller::Both,
            Command::TRES,
            [0x04, 0xB0, 0x03, 0x20], // 0x4B0 * 0x320 (1200 * 800) each
        )
        .await?;
        self.command(
            spi,
            Controller::Master,
            Command::PowerSetting,
            [0x0F, 0x00, 0x28, 0x2C, 0x28, 0x38],
        )
        .await?;
        self.command(spi, Controller::Master, Command::CmdB6, [0x07])
            .await?;
        self.command(
            spi,
            Controller::Master,
            Command::BoosterSoftStart2,
            [0xE0, 0x20],
        )
        .await?;
        self.command(spi, Controller::Master, Command::CmdB7, [0x01])
            .await?;
        self.command(
            spi,
            Controller::Master,
            Command::BoosterSoftStart1,
            [0xE0, 0x20],
        )
        .await?;
        self.command(spi, Controller::Master, Command::CmdB0, [0x01])
            .await?;
        self.command(spi, Controller::Master, Command::CmdB1, [0x02])
            .await?;
        Ok(())
    }

    async fn power_on(&mut self, spi: &mut SPI) -> Result<(), Self::Error> {
        self.command(spi, Controller::Both, Command::PowerOn, [])
            .await?;
        self.wait_until_idle().await?;
        Ok(())
    }

    async fn power_off(&mut self, spi: &mut SPI) -> Result<(), Self::Error> {
        self.command(spi, Controller::Both, Command::PowerOff, [0x00])
            .await?;
        self.wait_until_idle().await?;
        Ok(())
    }

    async fn update_frame(
        &mut self,
        spi: &mut SPI,
        pixels: impl IntoIterator<Item = Self::Color> + Clone,
    ) -> Result<(), Self::Error> {
        self.command(spi, Controller::Both, Command::CCSET, [0x01])
            .await?;
        self.wait_until_idle().await?;
        let mut data = SpectraPacker(pixels.into_iter());
        self.command(
            spi,
            Controller::Master,
            Command::DataStartTransmission,
            data.by_ref().take(PANEL_WIDTH * PANEL_HEIGHT / 4),
        )
        .await?;
        self.command(spi, Controller::Slave, Command::DataStartTransmission, data)
            .await?;
        Ok(())
    }

    async fn display_frame_no_wait(&mut self, spi: &mut SPI) -> Result<(), Self::Error> {
        self.command(spi, Controller::Both, Command::DisplayRefresh, [0x01])
            .await
    }

    async fn wait_until_idle(&mut self) -> Result<(), Self::Error> {
        self.busy
            .wait_for_high()
            .await
            .map_err(T133A01Error::BUSYError)
    }
}
