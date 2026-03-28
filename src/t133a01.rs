use crate::displayinterface::{DisplayInterfaceAsync, DisplayInterfaceAsyncError};
use crate::spectra6::{Spectra6Color, SpectraPacker};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiDevice;
use esp_println::println;

const SINGLE_BYTE_WRITE: bool = true;
const IS_BUSY_LOW: bool = true;

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

impl crate::displayinterface::Command for Command {
    fn address(self) -> u8 {
        self as u8
    }
}

pub struct T133A01<SPI, BUSY, DC, RST, DELAY, CS_SLAVE> {
    interface: DisplayInterfaceAsync<SPI, BUSY, DC, RST, DELAY, SINGLE_BYTE_WRITE>,
    cs_slave: CS_SLAVE,
}

impl<SPI, BUSY, DC, RST, DELAY, CS_SLAVE> T133A01<SPI, BUSY, DC, RST, DELAY, CS_SLAVE>
where
    SPI: SpiDevice,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
    DELAY: DelayNs,
    CS_SLAVE: OutputPin,
{
    pub fn new(_: &mut SPI, busy: BUSY, dc: DC, rst: RST, _: &mut DELAY, cs_slave: CS_SLAVE) -> Self {
        let mut cs_slave = cs_slave;
        cs_slave.set_high().unwrap();
        T133A01 {
            interface: DisplayInterfaceAsync::new(busy, dc, rst),
            cs_slave,
        }
    }

    pub async fn reset(
        &mut self,
        delay: &mut DELAY,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        // TODO: Can I lower these to 10_000?
        self.interface.reset(delay, 20_000, 20_000, 20_000).await
    }

    async fn cmd_with_data(&mut self, spi: &mut SPI, cmd: Command, data: &[u8]) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.interface.cmd_with_data(spi, cmd, data).await
    }

    async fn cmd_with_data_mirror(&mut self, spi: &mut SPI, cmd: Command, data: &[u8]) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.cs_slave.set_low().unwrap();
        let ret = self.interface.cmd_with_data(spi, cmd, data).await;
        self.cs_slave.set_high().unwrap();
        ret
    }

    pub async fn init(
        &mut self,
        spi: &mut SPI,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        println!("Busy 0: {:?}", self.interface.is_busy());
        // NOTE: Call after reset
        self
            .cmd_with_data(
                spi,
                Command::Cmd74,
                &[0x00, 0x0C, 0x0C, 0xD9, 0xDD, 0xDD, 0x15, 0x15, 0x55],
            )
            .await?;
        println!("Busy 1: {:?}", self.interface.is_busy());
        self
            .cmd_with_data_mirror(spi, Command::CmdF0, &[0x49, 0x55, 0x13, 0x5D, 0x05, 0x10])
            .await?;
        println!("Busy 2: {:?}", self.interface.is_busy());
        self.wait_until_idle().await?;
        self
            .cmd_with_data_mirror(spi, Command::PanelSetting, &[0xDF, 0x69])
            .await?;
        println!("Busy 3: {:?}", self.interface.is_busy());
        self
            .cmd_with_data(spi, Command::DCDC, &[0x44, 0x54, 0x00])
            .await?;
        println!("Busy 4: {:?}", self.interface.is_busy());
        self
            .cmd_with_data_mirror(spi, Command::CDI, &[0x37])
            .await?;
        println!("Busy 5: {:?}", self.interface.is_busy());
        self
            .cmd_with_data_mirror(spi, Command::Cmd60, &[0x03, 0x03])
            .await?;
        println!("Busy 6: {:?}", self.interface.is_busy());
        self
            .cmd_with_data_mirror(spi, Command::Cmd86, &[0x10])
            .await?;
        println!("Busy 7: {:?}", self.interface.is_busy());
        self
            .cmd_with_data_mirror(spi, Command::PWS, &[0x22])
            .await?;
        println!("Busy 8: {:?}", self.interface.is_busy());
        self
            .cmd_with_data_mirror(spi, Command::TRES, &[0x04, 0xB0, 0x03, 0x20])
            .await?;
        println!("Busy 9: {:?}", self.interface.is_busy());
        self
            .cmd_with_data(
                spi,
                Command::PowerSetting,
                &[0x0F, 0x00, 0x28, 0x2C, 0x28, 0x38],
            )
            .await?;
        println!("Busy 10: {:?}", self.interface.is_busy());
        self
            .cmd_with_data(spi, Command::CmdB6, &[0x07])
            .await?;
        println!("Busy 11: {:?}", self.interface.is_busy());
        self
            .cmd_with_data(spi, Command::BoosterSoftStart2, &[0xE0, 0x20])
            .await?;
        println!("Busy 12: {:?}", self.interface.is_busy());
        self
            .cmd_with_data(spi, Command::CmdB7, &[0x01])
            .await?;
        println!("Busy 13: {:?}", self.interface.is_busy());
        self
            .cmd_with_data(spi, Command::BoosterSoftStart1, &[0xE0, 0x20])
            .await?;
        println!("Busy 14: {:?}", self.interface.is_busy());
        self
            .cmd_with_data(spi, Command::CmdB0, &[0x01])
            .await?;
        println!("Busy 15: {:?}", self.interface.is_busy());
        self
            .cmd_with_data(spi, Command::CmdB1, &[0x02])
            .await?;
        println!("Busy 16: {:?}", self.interface.is_busy());
        Ok(())
    }
    pub async fn wait_until_idle(
        &mut self,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.interface.wait_until_idle(IS_BUSY_LOW).await
    }

    pub async fn update_frame_raw(
        &mut self,
        spi: &mut SPI,
        data: impl IntoIterator<Item = u8>,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.cmd_with_data_mirror(spi, Command::CCSET, &[0x01]).await?;
        self.wait_until_idle().await?;
        self.cs_slave.set_low().unwrap();
        self.interface
            .cmd(spi, Command::DataStartTransmission)
            .await?;
        self.interface.data_iter(spi, data).await?;
        self.cs_slave.set_high().unwrap();
        Ok(())
    }

    pub async fn update_frame(
        &mut self,
        spi: &mut SPI,
        pixels: impl IntoIterator<Item = Spectra6Color>,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.update_frame_raw(spi, SpectraPacker(pixels.into_iter()))
            .await
    }

    pub async fn display_frame(
        &mut self,
        spi: &mut SPI,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.cs_slave.set_low().unwrap();
        let ret = self.interface
            .cmd_with_data(spi, Command::DisplayRefresh, &[0x00])
            .await;
        self.cs_slave.set_high().unwrap();
        ret
        // NOTE: Must wait here
    }
    pub async fn power_on(
        &mut self,
        spi: &mut SPI,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.cs_slave.set_low().unwrap();
        let ret = self.interface.cmd(spi, Command::PowerOn).await;
        self.cs_slave.set_high().unwrap();
        ret
        // NOTE: Must wait here
    }

    pub async fn power_off(
        &mut self,
        spi: &mut SPI,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.cs_slave.set_low().unwrap();
        let ret = self.interface
            .cmd_with_data(spi, Command::PowerOff, &[0x00])
            .await;
        self.cs_slave.set_high().unwrap();
        ret
        //NOTE: Must wait here
    }
}
