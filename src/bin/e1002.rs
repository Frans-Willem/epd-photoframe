#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use embassy_executor::Spawner;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pin, Pull};
use esp_hal::spi::Mode as SpiMode;
use esp_hal::spi::master::Config as SpiConfig;
use esp_hal::spi::master::Spi;

extern crate alloc;

use epd_photoframe::app::App;
use epd_photoframe::panel::gdep073e01::Gdep073e01;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let reset_reason = esp_hal::rtc_cntl::reset_reason(esp_hal::system::Cpu::ProCpu);
    let wake_reason = esp_hal::rtc_cntl::wakeup_cause();
    let peripherals = epd_photoframe::app::init();

    let (gpio_btn_refresh, gpio_btn_previous, gpio_btn_next) =
        (peripherals.GPIO3, peripherals.GPIO5, peripherals.GPIO4);

    let epd_spi_bus = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_write_bit_order(esp_hal::spi::BitOrder::MsbFirst)
            .with_frequency(esp_hal::time::Rate::from_mhz(20))
            .with_mode(SpiMode::_0),
    )
    .unwrap();

    let mut epd_spi_bus = epd_spi_bus
        .with_sck(peripherals.GPIO7)
        .with_mosi(peripherals.GPIO9)
        .into_async();

    let epd = Gdep073e01::new(
        &mut epd_spi_bus,
        Output::new(peripherals.GPIO10, Level::Low, OutputConfig::default()),
        Input::new(
            peripherals.GPIO13,
            InputConfig::default().with_pull(Pull::Up),
        ),
        Output::new(peripherals.GPIO11, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default()),
    );

    App {
        spawner,
        reset_reason,
        wake_reason,
        wifi: peripherals.WIFI,
        flash: peripherals.FLASH,
        psram: peripherals.PSRAM,
        lpwr: peripherals.LPWR,
        timg0: peripherals.TIMG0,
        sw_interrupt: peripherals.SW_INTERRUPT,
        status_led: peripherals.GPIO6.degrade(),
        refresh_button_label: "Refresh button (green)",
        has_sy6974b: false,
        i2c0: peripherals.I2C0,
        gpio_i2c_sda: peripherals.GPIO19,
        gpio_i2c_scl: peripherals.GPIO20,
        adc1: peripherals.ADC1,
        gpio_battery_enable: peripherals.GPIO21,
        gpio_battery_sense: peripherals.GPIO1,
        gpio_btn_refresh: gpio_btn_refresh.degrade(),
        gpio_btn_previous: gpio_btn_previous.degrade(),
        gpio_btn_next: gpio_btn_next.degrade(),
        spi_bus: epd_spi_bus,
        epd,
        buzzer: epd_photoframe::buzzer::Buzzer::new(peripherals.LEDC, peripherals.GPIO45),
    }
    .run()
    .await
}
