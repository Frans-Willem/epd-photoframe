#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;

use esp_hal::gpio::{Input, InputConfig, Pin, Pull};
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_println::println;

use esp_hal::spi::master::Config as SpiConfig;
use esp_hal::spi::master::Spi;
use esp_hal::spi::Mode as SpiMode;

use esp_hal::system::SleepSource;

extern crate alloc;

use epd_photoframe::config::Config;
use epd_photoframe::config_mode;
use epd_photoframe::hardware::{HardwareCtx, WakeAction};
use epd_photoframe::panel::{Panel, PanelColor};
use epd_photoframe::panic_mode;

#[cfg(feature = "e1002")]
use epd_photoframe::panel::gdep073e01::Gdep073e01;
#[cfg(feature = "e1001")]
use epd_photoframe::panel::gdey075t7::Gdey075t7;
#[cfg(feature = "e1004")]
use epd_photoframe::panel::t133a01::T133A01;

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

fn determine_wake_action(
    reset_reason: Option<esp_hal::rtc_cntl::SocResetReason>,
    wake_reason: SleepSource,
    refresh_latched: bool,
    previous_latched: bool,
    next_latched: bool,
) -> WakeAction {
    // A `CoreSw` reset paired with a populated `PENDING_ACTION` slot is
    // the abort-during-refresh handoff from `normal_mode::run`. Honour
    // it and short-circuit. Any other reset reason clears the slot so a
    // value left behind by a previous abort can't leak through if the
    // following soft reset never happened (e.g. brownout / watchdog).
    if reset_reason == Some(esp_hal::rtc_cntl::SocResetReason::CoreSw) {
        if let Some(action) = epd_photoframe::normal_mode::PENDING_ACTION.take() {
            println!(
                "Resuming with pending action {:?} from previous cycle",
                action
            );
            return action;
        }
    } else {
        epd_photoframe::normal_mode::PENDING_ACTION.clear();
    }

    match wake_reason {
        SleepSource::Undefined => WakeAction::FreshBoot,
        SleepSource::Timer => WakeAction::Timer,
        // `RtcioWakeupSource` on the ESP32-S3 reports as `Gpio`. The
        // RTC-IO interrupt latch authoritatively tells us which button(s)
        // triggered the wake, even if the user has already released.
        SleepSource::Gpio => {
            if refresh_latched {
                WakeAction::Refresh
            } else if next_latched {
                WakeAction::Next
            } else if previous_latched {
                WakeAction::Previous
            } else {
                println!(
                    "WARNING: Gpio wake with no button bit latched; \
                     defaulting to Refresh"
                );
                WakeAction::Refresh
            }
        }
        other => {
            println!(
                "WARNING: unexpected wake reason {:?}; treating as fresh boot",
                other
            );
            WakeAction::FreshBoot
        }
    }
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

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    esp_alloc::psram_allocator!(
        peripherals.PSRAM,
        esp_hal::psram,
        esp_hal::psram::PsramConfig {
            mode: esp_hal::psram::PsramMode::OctalSpi,
            ..Default::default()
        }
    );

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_ints =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_ints.software_interrupt0);

    // Bind semantic board pins to device-specific GPIOs. This is the
    // only place where the silkscreen-to-GPIO and status LED mappings
    // appear; everything downstream uses the semantic local names.
    // E1001 inherits the E1002 mapping (same hardware aside from the panel).
    #[cfg(any(feature = "e1001", feature = "e1002"))]
    let (gpio_btn_refresh, gpio_btn_previous, gpio_btn_next, led_pin) = (
        peripherals.GPIO3,
        peripherals.GPIO5,
        peripherals.GPIO4,
        peripherals.GPIO6,
    );
    #[cfg(feature = "e1004")]
    let (gpio_btn_refresh, gpio_btn_previous, gpio_btn_next, led_pin) = (
        peripherals.GPIO5,
        peripherals.GPIO4,
        peripherals.GPIO3,
        peripherals.GPIO48,
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

    #[cfg(any(feature = "e1001", feature = "e1002"))]
    let mut epd_spi_bus = epd_spi_bus
        .with_sck(peripherals.GPIO7)
        .with_mosi(peripherals.GPIO9)
        .into_async();

    #[cfg(feature = "e1004")]
    let mut epd_spi_bus = epd_spi_bus
        .with_sck(peripherals.GPIO7)
        .with_miso(peripherals.GPIO8)
        .with_mosi(peripherals.GPIO9)
        .into_async();

    #[cfg(feature = "e1001")]
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

    #[cfg(feature = "e1002")]
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

    #[cfg(feature = "e1004")]
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

    // PPK2 measurement mode: park the SY6974B charger in HIZ before
    // anything else runs so the system rail spends as little time as
    // possible drawing through VBUS. This must happen after I²C0 is up
    // (the chip lives on this bus).
    #[cfg(all(feature = "e1004", feature = "disable_charger"))]
    let i2c0 = epd_photoframe::sy6974b::enter_measurement_mode(i2c0).await;

    // Snapshot the RTC-IO wake latch (which pin actually triggered the wake)
    // and clear it immediately so stale bits don't carry into the next cycle.
    // The latch is authoritative because it captures the pin state at the
    // exact moment of wake, even if the user releases the button during
    // the ~300 ms bootloader window.
    let button_bits = (1u32 << gpio_btn_refresh.number())
        | (1u32 << gpio_btn_previous.number())
        | (1u32 << gpio_btn_next.number());
    let rtc_gpio_int_mask = read_and_clear_rtc_gpio_wake_status(button_bits);

    let refresh_latched = (rtc_gpio_int_mask & (1u32 << gpio_btn_refresh.number())) != 0;
    let previous_latched = (rtc_gpio_int_mask & (1u32 << gpio_btn_previous.number())) != 0;
    let next_latched = (rtc_gpio_int_mask & (1u32 << gpio_btn_next.number())) != 0;

    let wake_action = determine_wake_action(
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

    spawner.spawn(blink_task(Output::new(led_pin, Level::Low, OutputConfig::default())).unwrap());

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
        battery_enable: Output::new(peripherals.GPIO21, Level::Low, OutputConfig::default()),
        adc1: peripherals.ADC1,
        battery_sense: peripherals.GPIO1,
        i2c0,
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
    if let Some(panic_msg) = panic_mode::take_pending_message() {
        panic_mode::run(hw, panic_msg.as_str()).await
    } else {
        run_normal_boot(hw, config).await
    }
}

/// The non-panic boot path: flip the panic-reboot guard on, kick off
/// the white pre-flash, run the config-mode hold race, and dispatch
/// to either `normal_mode::run` (we have credentials and the user isn't
/// holding both Previous + Next) or `config_mode::run`. Factored out
/// of `main()` so the panic-render fast path's divergence reads as a
/// clean two-arm `if let … else …` at the call site rather than
/// "one arm calls a `-> !` function and the rest of the function
/// implicitly only runs on the other arm".
async fn run_normal_boot<P>(mut hw: HardwareCtx<P>, config: Config<'static>) -> !
where
    P: Panel<Spi<'static, esp_hal::Async>>,
{
    // Past the panic-render decision: any panic from here on is
    // happening in a "normal" cycle, so the handler is allowed to
    // stash + soft-reset (release builds) instead of halting. See
    // `panic_mode::REBOOT_ALLOWED` for the why.
    panic_mode::allow_reboot();

    // --- White pre-flash (non-blocking) for immediate user feedback ---
    //
    // Kicked off before we decide which flow to run so the user sees the
    // panel start updating as soon as the device wakes, even while the
    // config-mode race is still counting down. Whichever flow wins will
    // reset the panel to draw its own content on top; the ~20 s refresh
    // just continues in the background until then.
    if hw.wake_action.show_white_flash() {
        println!("White pre-flash");
        // Bring the panel's enable rail up before any panel I/O.
        // `enable()` is idempotent (sets EN high), so a later flow
        // (`normal_mode::run`, `config_mode::run`) re-asserting it is fine.
        hw.epd.enable().await.unwrap();
        println!("Reset");
        hw.epd.reset().await.unwrap();
        println!("Wait until idle");
        hw.epd.wait_until_idle().await.unwrap();
        println!("Init");
        // Ask the panel which init mode covers an all-white palette;
        // on E1001 that's `Bw`, single-mode panels return `()`.
        let init_mode = P::init_mode_for_palette([P::Color::WHITE]);
        hw.epd.init(&mut hw.spi_bus, init_mode).await.unwrap();
        println!("Power on");
        hw.epd.power_on(&mut hw.spi_bus).await.unwrap();
        println!("Update frame (white)");
        hw.epd
            .update_frame(
                &mut hw.spi_bus,
                (0..(P::WIDTH * P::HEIGHT)).map(|_| P::Color::WHITE),
            )
            .await
            .unwrap();
        println!("Trigger refresh (no wait)");
        hw.epd.display_frame_no_wait(&mut hw.spi_bus).await.unwrap();
    }

    // If NVS didn't produce a full set of credentials we go straight to
    // config mode without the button race. Otherwise: race a 10-second
    // timer against either Previous or Next being released — if both were
    // held at boot and stay held for the whole window, the timer wins and
    // we enter configuration mode. If either (or both) was never pressed,
    // `wait_for_high` resolves immediately because the pin is already high,
    // so normally this block completes in microseconds.
    let entering_config_mode = !config.is_configured().unwrap_or(false) || {
        let mut prev_input = Input::new(
            hw.gpio_btn_previous.reborrow(),
            InputConfig::default().with_pull(Pull::Up),
        );
        let mut next_input = Input::new(
            hw.gpio_btn_next.reborrow(),
            InputConfig::default().with_pull(Pull::Up),
        );
        matches!(
            embassy_futures::select::select3(
                prev_input.wait_for_high(),
                next_input.wait_for_high(),
                Timer::at(Instant::MIN + Duration::from_secs(10)),
            )
            .await,
            embassy_futures::select::Either3::Third(_)
        )
    };

    if !entering_config_mode {
        epd_photoframe::normal_mode::run(hw, config).await
    } else {
        // Entering config mode is a deliberate reset of the device's
        // "what's displayed" state — whatever URL was committed last
        // cycle (and any pending redirect) is no longer relevant once
        // the user has reconfigured.
        epd_photoframe::normal_mode::CURRENT_URL.clear();
        epd_photoframe::normal_mode::REDIRECT_URL.clear();
        config_mode::run(hw, config).await
    }
}
