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

pub enum T133A01Error<Spi, CsMaster, CsSlave, Busy, Dc, Rst, En>
where
    Spi: SpiBus,
    CsMaster: OutputPin,
    CsSlave: OutputPin,
    Busy: InputPin + Wait,
    Dc: OutputPin,
    Rst: OutputPin,
    En: OutputPin,
{
    SpiError(Spi::Error),
    CsMasterError(CsMaster::Error),
    CsSlaveError(CsSlave::Error),
    BusyError(Busy::Error),
    DcError(Dc::Error),
    RstError(Rst::Error),
    EnError(En::Error),
}

impl<Spi, CsMaster, CsSlave, Busy, Dc, Rst, En> core::fmt::Debug
    for T133A01Error<Spi, CsMaster, CsSlave, Busy, Dc, Rst, En>
where
    Spi: SpiBus,
    CsMaster: OutputPin,
    CsSlave: OutputPin,
    Busy: InputPin + Wait,
    Dc: OutputPin,
    Rst: OutputPin,
    En: OutputPin,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SpiError(x) => write!(f, "SpiError({:?})", x),
            Self::CsMasterError(x) => write!(f, "CsMasterError({:?})", x),
            Self::CsSlaveError(x) => write!(f, "CsSlaveError({:?})", x),
            Self::BusyError(x) => write!(f, "BusyError({:?})", x),
            Self::DcError(x) => write!(f, "DcError({:?})", x),
            Self::RstError(x) => write!(f, "RstError({:?})", x),
            Self::EnError(x) => write!(f, "EnError({:?})", x),
        }
    }
}

pub struct T133A01<Spi, CsMaster, CsSlave, Busy, Dc, Rst, En> {
    _spi: PhantomData<Spi>,
    cs_master: CsMaster,
    cs_slave: CsSlave,
    busy: Busy,
    dc: Dc,
    rst: Rst,
    en: En,
}

impl<Spi, CsMaster, CsSlave, Busy, Dc, Rst, En>
    T133A01<Spi, CsMaster, CsSlave, Busy, Dc, Rst, En>
where
    CsMaster: OutputPin,
    CsSlave: OutputPin,
{
    /// `_spi` is taken only to fix `Spi` at the call site without
    /// requiring a turbofish; the bus itself isn't stored.
    pub fn new(
        _spi: &mut Spi,
        cs_master: CsMaster,
        cs_slave: CsSlave,
        busy: Busy,
        dc: Dc,
        rst: Rst,
        en: En,
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

impl<Spi, CsMaster, CsSlave, Busy, Dc, Rst, En>
    T133A01<Spi, CsMaster, CsSlave, Busy, Dc, Rst, En>
where
    Spi: SpiBus,
    CsMaster: OutputPin,
    CsSlave: OutputPin,
    Busy: InputPin + Wait,
    Dc: OutputPin,
    Rst: OutputPin,
    En: OutputPin,
{
    pub fn is_busy(&mut self) -> bool {
        self.busy.is_low().unwrap()
    }

    async fn command(
        &mut self,
        spi: &mut Spi,
        controller: Controller,
        command: Command,
        data: impl IntoIterator<Item = u8>,
    ) -> Result<(), T133A01Error<Spi, CsMaster, CsSlave, Busy, Dc, Rst, En>> {
        // Assert chip select, pull low
        match controller {
            Controller::Master => self
                .cs_master
                .set_low()
                .map_err(T133A01Error::CsMasterError)?,
            Controller::Slave => self
                .cs_slave
                .set_low()
                .map_err(T133A01Error::CsSlaveError)?,
            Controller::Both => {
                self.cs_master
                    .set_low()
                    .map_err(T133A01Error::CsMasterError)?;
                self.cs_slave
                    .set_low()
                    .map_err(T133A01Error::CsSlaveError)?;
            }
        };
        // Write command
        self.dc.set_high().map_err(T133A01Error::DcError)?;
        spi.write(&[command.into()])
            .await
            .map_err(T133A01Error::SpiError)?;
        // Write data
        self.dc.set_low().map_err(T133A01Error::DcError)?;
        if SINGLE_BYTE_WRITE {
            for val in data.into_iter() {
                spi.write(&[val]).await.map_err(T133A01Error::SpiError)?;
            }
        } else {
            for chunk in data.into_iter().chunks_heapless::<128>() {
                spi.write(&chunk).await.map_err(T133A01Error::SpiError)?;
            }
        }
        // Deassert chip select, pull high
        match controller {
            Controller::Master => self
                .cs_master
                .set_high()
                .map_err(T133A01Error::CsMasterError)?,
            Controller::Slave => self
                .cs_slave
                .set_high()
                .map_err(T133A01Error::CsSlaveError)?,
            Controller::Both => {
                self.cs_master
                    .set_high()
                    .map_err(T133A01Error::CsMasterError)?;
                self.cs_slave
                    .set_high()
                    .map_err(T133A01Error::CsSlaveError)?;
            }
        };
        Ok(())
    }
}

impl<Spi, CsMaster, CsSlave, Busy, Dc, Rst, En> Panel<Spi>
    for T133A01<Spi, CsMaster, CsSlave, Busy, Dc, Rst, En>
where
    Spi: SpiBus,
    CsMaster: OutputPin,
    CsSlave: OutputPin,
    Busy: InputPin + Wait,
    Dc: OutputPin,
    Rst: OutputPin,
    En: OutputPin,
{
    type Color = Spectra6Color;
    type Error = T133A01Error<Spi, CsMaster, CsSlave, Busy, Dc, Rst, En>;
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
        self.en.set_high().map_err(T133A01Error::EnError)
    }

    async fn disable(&mut self) -> Result<(), Self::Error> {
        self.en.set_low().map_err(T133A01Error::EnError)
    }

    async fn reset(&mut self) -> Result<(), Self::Error> {
        // TODO: Can I lower these to 10ms?
        self.rst.set_high().map_err(T133A01Error::RstError)?;
        Timer::after(Duration::from_millis(20)).await;
        self.rst.set_low().map_err(T133A01Error::RstError)?;
        Timer::after(Duration::from_millis(20)).await;
        self.rst.set_high().map_err(T133A01Error::RstError)?;
        Timer::after(Duration::from_millis(20)).await;
        Ok(())
    }

    async fn init(&mut self, spi: &mut Spi, _mode: Self::InitMode) -> Result<(), Self::Error> {
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

    async fn power_on(&mut self, spi: &mut Spi) -> Result<(), Self::Error> {
        self.command(spi, Controller::Both, Command::PowerOn, [])
            .await?;
        self.wait_until_idle().await?;
        Ok(())
    }

    async fn power_off(&mut self, spi: &mut Spi) -> Result<(), Self::Error> {
        self.command(spi, Controller::Both, Command::PowerOff, [0x00])
            .await?;
        self.wait_until_idle().await?;
        Ok(())
    }

    async fn update_frame(
        &mut self,
        spi: &mut Spi,
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

    async fn display_frame_no_wait(&mut self, spi: &mut Spi) -> Result<(), Self::Error> {
        self.command(spi, Controller::Both, Command::DisplayRefresh, [0x01])
            .await
    }

    async fn wait_until_idle(&mut self) -> Result<(), Self::Error> {
        self.busy
            .wait_for_high()
            .await
            .map_err(T133A01Error::BusyError)
    }
}
