#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use embassy_executor::Spawner;
use esp_hal::clock::CpuClock;

use esp_hal::gpio::{Input, InputConfig, Pin, Pull};
use esp_hal::gpio::{Level, Output, OutputConfig};

use esp_hal::spi::Mode as SpiMode;
use esp_hal::spi::master::Config as SpiConfig;
use esp_hal::spi::master::Spi;
use esp_hal::uart::{Config as UartConfig, UartRx};

extern crate alloc;

use epd_photoframe::app::{AppHardware, init_runtime, run_app};
use epd_photoframe::panel::t133a01::T133A01;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    init_runtime(
        peripherals.PSRAM,
        peripherals.TIMG0,
        peripherals.SW_INTERRUPT,
    );

    let (gpio_btn_refresh, gpio_btn_previous, gpio_btn_next, led_pin) = (
        peripherals.GPIO5,
        peripherals.GPIO4,
        peripherals.GPIO3,
        peripherals.GPIO48,
    );

    let (refresh_button_label, has_sy6974b) = ("Refresh button", true);
    let uart_rx = UartRx::new(peripherals.UART0, UartConfig::default())
        .unwrap()
        .with_rx(peripherals.GPIO44)
        .into_async();

    // --- Build the panel SPI bus and EPD driver (shared by both flows) ---
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
        .with_miso(peripherals.GPIO8)
        .with_mosi(peripherals.GPIO9)
        .into_async();

    let epd = T133A01::new(
        &mut epd_spi_bus,
        Output::new(peripherals.GPIO10, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO2, Level::Low, OutputConfig::default()),
        Input::new(
            peripherals.GPIO13,
            InputConfig::default().with_pull(Pull::Up),
        ),
        Output::new(peripherals.GPIO11, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO38, Level::Low, OutputConfig::default()),
        // GPIO12: TFT_EN board rail (E1004 only); high while the panel is powered.
        Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default()),
    );

    // One task later drives both per-wake sensor reads (battery ADC +
    // SHT40 over I²C0) concurrently via `join`. I²C0 is built early so
    // the optional charger measurement-mode setup can also happen before
    // the generic boot flow below.
    let i2c0 =
        esp_hal::i2c::master::I2c::new(peripherals.I2C0, esp_hal::i2c::master::Config::default())
            .unwrap()
            .with_sda(peripherals.GPIO19)
            .with_scl(peripherals.GPIO20)
            .into_async();

    // Power-measurement mode: park the SY6974B charger in HIZ before
    // anything else runs so the system rail spends as little time as
    // possible drawing through VBUS. This must happen after I²C0 is up
    // (the chip lives on this bus). Other boards still support generic
    // power-measurement behavior, but do not have an accessible SY6974B.
    #[cfg(feature = "power_measurement")]
    let i2c0 = epd_photoframe::sy6974b::enter_measurement_mode(i2c0).await;

    let board = AppHardware {
        gpio_btn_refresh: gpio_btn_refresh.degrade(),
        gpio_btn_previous: gpio_btn_previous.degrade(),
        gpio_btn_next: gpio_btn_next.degrade(),
        led_pin: led_pin.degrade(),
        refresh_button_label,
        has_sy6974b,
        uart_rx,
        spi_bus: epd_spi_bus,
        epd,
        i2c0,
        flash: peripherals.FLASH,
        lpwr: peripherals.LPWR,
        wifi: peripherals.WIFI,
        battery_enable_pin: peripherals.GPIO21,
        adc1: peripherals.ADC1,
        battery_sense: peripherals.GPIO1,
        ledc: peripherals.LEDC,
        buzzer_pin: peripherals.GPIO45,
    };

    run_app(spawner, board).await
}
