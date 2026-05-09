#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;

use esp_hal::gpio::{Input, InputConfig, Pin, Pull};
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_println::println;

use esp_hal::spi::Mode as SpiMode;
use esp_hal::spi::master::Config as SpiConfig;
use esp_hal::spi::master::Spi;

extern crate alloc;

use epd_photoframe::config::Config;
use epd_photoframe::hardware::HardwareCtx;
use epd_photoframe::panel::gdey075t7::Gdey075t7;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

/// Read the RTC-IO interrupt status register (the wake latch) masked to the
/// caller's bits of interest, clear those same bits via the register's
/// write-1-to-clear sibling, and return the pre-clear masked value. Other
/// bits in the register are neither read back nor touched.
///
/// Wraps the single `unsafe` the PAC mandates for this register: svd2rust
/// marks `RTC_GPIO_STATUS_W1TC` with `Safety = Unsafe` because it can't
/// statically verify field-value semantics, but writing any `u32` mask to
/// a write-1-to-clear status register only flips hardware status bits in
/// the RTC domain and cannot violate memory safety.
fn read_and_clear_rtc_gpio_wake_status(mask: u32) -> u32 {
    let rtc_io = esp_hal::peripherals::RTC_IO::regs();
    let value = rtc_io.rtc_gpio_status().read().int().bits() & mask;
    unsafe {
        rtc_io
            .rtc_gpio_status_w1tc()
            .write(|w| w.rtc_gpio_status_int_w1tc().bits(mask));
    }
    value
}

#[embassy_executor::task]
async fn blink_task(mut led: Output<'static>) {
    loop {
        led.toggle();
        Timer::after(Duration::from_millis(500)).await;
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let reset_reason = esp_hal::rtc_cntl::reset_reason(esp_hal::system::Cpu::ProCpu);
    let wake_reason = esp_hal::rtc_cntl::wakeup_cause();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Bind semantic button names to the device-specific GPIOs. This is the
    // only place where the silkscreen-to-GPIO mapping appears; everything
    // downstream uses the `gpio_btn_*` handles (and their `.number()`) so
    // the rest of main() is device-agnostic. E1001 inherits the E1002
    // mapping (same hardware aside from the panel).
    let (gpio_btn_refresh, gpio_btn_previous, gpio_btn_next) =
        (peripherals.GPIO3, peripherals.GPIO5, peripherals.GPIO4);

    // Snapshot the RTC-IO wake latch (which pin actually triggered the wake)
    // and clear it immediately so stale bits don't carry into the next cycle.
    // The latch is authoritative because it captures the pin state at the
    // exact moment of wake — even a sub-millisecond tap is recorded.
    let button_bits = (1u32 << gpio_btn_refresh.number())
        | (1u32 << gpio_btn_previous.number())
        | (1u32 << gpio_btn_next.number());
    let rtc_gpio_int_mask = read_and_clear_rtc_gpio_wake_status(button_bits);

    let refresh_latched = (rtc_gpio_int_mask & (1u32 << gpio_btn_refresh.number())) != 0;
    let previous_latched = (rtc_gpio_int_mask & (1u32 << gpio_btn_previous.number())) != 0;
    let next_latched = (rtc_gpio_int_mask & (1u32 << gpio_btn_next.number())) != 0;

    let wake_action = epd_photoframe::app::determine_wake_action(
        reset_reason,
        wake_reason,
        refresh_latched,
        previous_latched,
        next_latched,
    );

    let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);
    let time_since_boot = rtc.time_since_power_up();
    println!(
        "Device booting up - reset={reset_reason:?} wake={wake_reason:?} action={wake_action:?} \
         latched[refresh={refresh_latched} previous={previous_latched} next={next_latched}] \
         uptime={time_since_boot:?}"
    );
    println!(
        "RTC CURRENT_URL: {:?}",
        epd_photoframe::normal_mode::CURRENT_URL.get().as_deref()
    );
    println!(
        "RTC REDIRECT_URL: {:?}",
        epd_photoframe::normal_mode::REDIRECT_URL.get().as_deref()
    );

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    esp_alloc::psram_allocator!(
        peripherals.PSRAM,
        esp_hal::psram,
        esp_hal::psram::PsramConfig {
            mode: esp_hal::psram::PsramMode::OctalSpi,
            ..Default::default()
        }
    );

    // Load runtime configuration from NVS. A fresh / blank partition is
    // not an error (esp-nvs treats all-0xFF as "no entries yet"), so the
    // only ways `Config::new` fails are programming bugs (wrong partition
    // offset/size) or actual flash hardware trouble — panicking there is
    // the right call. A missing *key* is different: that's how we detect
    // "needs configuring" and short-circuit into config mode below.
    let config =
        Config::new(peripherals.FLASH).expect("NVS init failed — check partition table and flash");
    if config.is_configured().unwrap_or(false) {
        let has_hint = config.get_wifi_hint().ok().flatten().is_some();
        println!(
            "Config in use: wifi.ssid={:?} wifi.pass=<{} chars> image.url={:?} wifi.hint={}",
            config
                .get_wifi_ssid()
                .ok()
                .flatten()
                .as_deref()
                .unwrap_or(""),
            config
                .get_wifi_password()
                .ok()
                .flatten()
                .map(|p| p.len())
                .unwrap_or(0),
            config
                .get_image_url()
                .ok()
                .flatten()
                .as_deref()
                .unwrap_or(""),
            if has_hint { "present" } else { "none" },
        );
    } else {
        println!("NVS config incomplete; forcing config mode");
    }

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_ints =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_ints.software_interrupt0);

    // Status LED is on a different GPIO per device.
    let led_pin = peripherals.GPIO6;
    spawner.spawn(blink_task(Output::new(led_pin, Level::Low, OutputConfig::default())).unwrap());

    // One task that drives both per-wake sensor reads (battery ADC +
    // SHT40 over I²C0) concurrently via `join`. By the time the URL
    // is being built in `normal_mode::run`, both signals have been
    // populated — the 10 ms ADC settle + the 10 ms SHT40 conversion
    // happen alongside the ~1.3 s WiFi association, so the `wait()`s
    // are essentially free.
    let i2c0 =
        esp_hal::i2c::master::I2c::new(peripherals.I2C0, esp_hal::i2c::master::Config::default())
            .unwrap()
            .with_sda(peripherals.GPIO19)
            .with_scl(peripherals.GPIO20)
            .into_async();

    // PPK2 measurement mode: park the SY6974B charger in HIZ before
    // anything else runs so the system rail spends as little time as
    // possible drawing through VBUS. This must happen after I²C0 is up
    // (the chip lives on this bus) but before any code that depends on
    // the BAT-only power path being established.
    spawner.spawn(
        epd_photoframe::normal_mode::sensor_task(
            Output::new(peripherals.GPIO21, Level::Low, OutputConfig::default()),
            peripherals.ADC1,
            peripherals.GPIO1,
            i2c0,
            false,
        )
        .unwrap(),
    );

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
        .with_mosi(peripherals.GPIO9)
        .into_async();

    let epd = Gdey075t7::new(
        &mut epd_spi_bus,
        Output::new(peripherals.GPIO10, Level::Low, OutputConfig::default()),
        Input::new(
            peripherals.GPIO13,
            InputConfig::default().with_pull(Pull::Up),
        ),
        Output::new(peripherals.GPIO11, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default()),
    );

    // Unified hardware context handed off to whichever top-level flow
    // wins (`panic_mode::run`, `run_normal_boot`, then `normal_mode::run`
    // or `config_mode::run`). The panel's board-level enable rail
    // (E1004's TFT_EN; no-op on E1002 / E1001) stays *off* here —
    // each flow asserts it (idempotently) before its own first panel
    // I/O, so the rail is only powered when the panel is about to be
    // touched.
    let hw = HardwareCtx {
        spawner,
        rtc,
        wake_action,
        wifi: peripherals.WIFI,
        refresh_button_label: "Refresh button (green)",
        has_sy6974b: false,
        gpio_btn_refresh: gpio_btn_refresh.degrade(),
        gpio_btn_previous: gpio_btn_previous.degrade(),
        gpio_btn_next: gpio_btn_next.degrade(),
        spi_bus: epd_spi_bus,
        epd,
        buzzer: epd_photoframe::buzzer::Buzzer::new(peripherals.LEDC, peripherals.GPIO45),
    };

    // --- Panic-render fast path -----------------------------------------
    //
    // If the previous boot ended in a panic, the release-build panic
    // handler stashed the formatted message in `panic_mode::PANIC_MSG`
    // and soft-reset. `take_pending_message` clears the slot, so a
    // re-panic during render falls back to a normal cycle on the next
    // boot rather than looping. Skips white pre-flash, WiFi, sensors,
    // and the config-mode race: just renders the message and deep-
    // sleeps, wakeable by timer or button so the user can retry
    // without a power-cycle. The `else` branch holds the rest of
    // `main()` so the divergence is structural — once we go into
    // `panic_mode::run` we never come back.
    epd_photoframe::app::run(hw, config).await
}
