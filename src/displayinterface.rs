use arrayvec::ArrayVec;
use core::fmt::Debug;
use core::marker::PhantomData;
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiDevice;

/* Maybe import from epd-waveshare? */
pub trait Command: Copy {
    fn address(self) -> u8;
}

pub enum DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>
where
    SPI: SpiDevice,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
{
    SPIError(SPI::Error),
    BUSYError(BUSY::Error),
    DCError(DC::Error),
    RSTError(RST::Error),
}

impl<SPI, BUSY, DC, RST> Debug for DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>
where
    SPI: SpiDevice,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SPIError(x) => write!(f, "SPIError({:?})", x),
            Self::BUSYError(x) => write!(f, "BUSYError({:?})", x),
            Self::DCError(x) => write!(f, "DCError({:?})", x),
            Self::RSTError(x) => write!(f, "RSTError({:?})", x),
        }
    }
}

pub struct DisplayInterfaceAsync<SPI, BUSY, DC, RST, DELAY, const SINGLE_BYTE_WRITE: bool> {
    _spi: PhantomData<SPI>,
    _delay: PhantomData<DELAY>,
    busy: BUSY,
    dc: DC,
    rst: RST,
}

impl<SPI, BUSY, DC, RST, DELAY, const SINGLE_BYTE_WRITE: bool>
    DisplayInterfaceAsync<SPI, BUSY, DC, RST, DELAY, SINGLE_BYTE_WRITE>
where
    SPI: SpiDevice,
    BUSY: InputPin + Wait,
    DC: OutputPin,
    RST: OutputPin,
    DELAY: DelayNs,
{
    pub fn new(busy: BUSY, dc: DC, rst: RST) -> Self {
        DisplayInterfaceAsync {
            _spi: PhantomData,
            _delay: PhantomData,
            busy,
            dc,
            rst,
        }
    }

    async fn write(&mut self, spi: &mut SPI, data: &[u8]) -> Result<(), SPI::Error> {
        // See description in epd-waveshare/src/interface.rs
        if cfg!(target_os = "linux") {
            for data_chunk in data.chunks(4096) {
                spi.write(data_chunk).await?;
            }
            Ok(())
        } else {
            spi.write(data).await
        }
    }

    async fn write_iter(
        &mut self,
        spi: &mut SPI,
        data: impl IntoIterator<Item = u8>,
    ) -> Result<(), SPI::Error> {
        let mut buffer = ArrayVec::<u8, 128>::new();
        for v in data.into_iter() {
            if buffer.is_full() {
                spi.write(buffer.as_slice()).await?;
                buffer.clear();
            }
            buffer.push(v);
        }
        if !buffer.is_empty() {
            spi.write(buffer.as_slice()).await?;
        }
        Ok(())
    }

    pub async fn cmd<T: Command>(
        &mut self,
        spi: &mut SPI,
        command: T,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.dc
            .set_low()
            .map_err(DisplayInterfaceAsyncError::DCError)?;
        self.write(spi, &[command.address()])
            .await
            .map_err(DisplayInterfaceAsyncError::SPIError)?;
        Ok(())
    }

    pub async fn data(
        &mut self,
        spi: &mut SPI,
        data: &[u8],
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.dc
            .set_high()
            .map_err(DisplayInterfaceAsyncError::DCError)?;
        if SINGLE_BYTE_WRITE {
            for val in data.iter().copied() {
                self.write(spi, &[val])
                    .await
                    .map_err(DisplayInterfaceAsyncError::SPIError)?;
            }
        } else {
            self.write(spi, data)
                .await
                .map_err(DisplayInterfaceAsyncError::SPIError)?;
        }
        Ok(())
    }

    pub async fn data_iter(
        &mut self,
        spi: &mut SPI,
        data: impl IntoIterator<Item = u8>,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.dc
            .set_high()
            .map_err(DisplayInterfaceAsyncError::DCError)?;
        if SINGLE_BYTE_WRITE {
            for val in data.into_iter() {
                self.write(spi, &[val])
                    .await
                    .map_err(DisplayInterfaceAsyncError::SPIError)?;
            }
        } else {
            self.write_iter(spi, data)
                .await
                .map_err(DisplayInterfaceAsyncError::SPIError)?;
        }
        Ok(())
    }

    pub async fn cmd_with_data<T: Command>(
        &mut self,
        spi: &mut SPI,
        command: T,
        data: &[u8],
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.cmd(spi, command).await?;
        self.data(spi, data).await
    }

    pub async fn data_x_times(
        &mut self,
        spi: &mut SPI,
        val: u8,
        repetitions: usize,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.data_iter(spi, (0..repetitions).map(|_| val)).await
    }

    pub async fn wait_until_idle(
        &mut self,
        is_busy_low: bool,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        // TODO: Add a proper timeout here?
        if is_busy_low {
            self.busy
                .wait_for_high()
                .await
                .map_err(DisplayInterfaceAsyncError::BUSYError)
        } else {
            self.busy
                .wait_for_low()
                .await
                .map_err(DisplayInterfaceAsyncError::BUSYError)
        }
    }

    pub fn is_busy(&mut self) -> bool {
        self.busy.is_high().unwrap()
    }

    pub async fn reset(
        &mut self,
        delay: &mut DELAY,
        initial_delay_us: u32,
        duration_us: u32,
        final_delay_us: u32,
    ) -> Result<(), DisplayInterfaceAsyncError<SPI, BUSY, DC, RST>> {
        self.rst
            .set_high()
            .map_err(DisplayInterfaceAsyncError::RSTError)?;
        delay.delay_us(initial_delay_us).await;
        self.rst
            .set_low()
            .map_err(DisplayInterfaceAsyncError::RSTError)?;
        delay.delay_us(duration_us).await;
        self.rst
            .set_high()
            .map_err(DisplayInterfaceAsyncError::RSTError)?;
        delay.delay_us(final_delay_us).await;
        Ok(())
    }
}
