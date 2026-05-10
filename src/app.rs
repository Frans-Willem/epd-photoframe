//! Generic application runtime shared by board-specific binaries.
//!
//! A binary is responsible for HAL setup and board-specific hardware
//! construction, then hands an [`AppHardware`] bundle to [`run_app`].

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::{AnyPin, Input, InputConfig, Level, Output, OutputConfig, Pin, Pull};
use esp_hal::spi::master::Spi;
use esp_hal::system::SleepSource;
use esp_hal::timer::timg::TimerGroup;
use esp_println::println;

use crate::buzzer::Buzzer;
use crate::config::Config;
use crate::config_mode;
use crate::panel::{Panel, PanelColor};
use crate::panic_mode;

/// What the user (or the wake timer) wants us to do this cycle. Consumed
/// by the normal flow to pick an `action=` query-string fragment and to
/// decide whether to run the white pre-flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeAction {
    /// Fresh power-on / cold reset. Show the white pre-flash, fetch with no action query.
    FreshBoot,
    /// Woken by the 10-minute timer. No pre-flash, fetch with no action query.
    Timer,
    /// Woken by the Refresh button (or by a GPIO wake where no button is still held).
    Refresh,
    Previous,
    Next,
}

impl WakeAction {
    /// Name of the `action=` query-string parameter to append to the
    /// base URL, or `None` if the base URL should be fetched unchanged.
    /// The caller is responsible for picking the right `?` / `&`
    /// separator depending on whether the base URL already carries a
    /// query string.
    pub fn action_name(self) -> Option<&'static str> {
        match self {
            WakeAction::Refresh => Some("refresh"),
            WakeAction::Previous => Some("previous"),
            WakeAction::Next => Some("next"),
            WakeAction::FreshBoot | WakeAction::Timer => None,
        }
    }

    /// Whether to trigger a non-blocking white pre-flash as immediate visual
    /// feedback that the press was seen. The 10-minute timer wake runs in the
    /// background, so no feedback is needed there.
    pub fn show_white_flash(self) -> bool {
        !matches!(self, WakeAction::Timer)
    }
}

/// Raw app hardware produced by board-specific setup before generic app
/// initialization has decoded wake state or built mode-level drivers.
pub struct AppHardware<P> {
    pub gpio_btn_refresh: AnyPin<'static>,
    pub gpio_btn_previous: AnyPin<'static>,
    pub gpio_btn_next: AnyPin<'static>,
    pub led_pin: AnyPin<'static>,
    pub refresh_button_label: &'static str,
    pub has_sy6974b: bool,
    pub spi_bus: Spi<'static, esp_hal::Async>,
    pub epd: P,
    pub i2c0: esp_hal::i2c::master::I2c<'static, esp_hal::Async>,
    pub flash: esp_hal::peripherals::FLASH<'static>,
    pub lpwr: esp_hal::peripherals::LPWR<'static>,
    pub wifi: esp_hal::peripherals::WIFI<'static>,
    pub battery_enable_pin: esp_hal::peripherals::GPIO21<'static>,
    pub adc1: esp_hal::peripherals::ADC1<'static>,
    pub battery_sense: esp_hal::peripherals::GPIO1<'static>,
    pub ledc: esp_hal::peripherals::LEDC<'static>,
    pub buzzer_pin: esp_hal::peripherals::GPIO45<'static>,
}

/// Runtime context handed from the generic app dispatcher to whichever
/// mode runs this boot. The panel driver is pre-built by `main`, and
/// the modes stay generic over the concrete driver so they don't need
/// cfg gates for pin counts or panel-model-specific types.
pub struct AppContext<P> {
    pub spawner: Spawner,
    pub rtc: esp_hal::rtc_cntl::Rtc<'static>,
    pub wake_action: WakeAction,
    pub wifi: esp_hal::peripherals::WIFI<'static>,
    pub refresh_button_label: &'static str,
    pub has_sy6974b: bool,

    /// Sensor hardware used only by the normal refresh flow to populate
    /// battery / temperature / humidity / charger status URL parameters.
    pub battery_enable: Output<'static>,
    pub adc1: esp_hal::peripherals::ADC1<'static>,
    pub battery_sense: esp_hal::peripherals::GPIO1<'static>,
    pub i2c0: esp_hal::i2c::master::I2c<'static, esp_hal::Async>,

    /// Button pins; held so the normal flow can hand them to
    /// `RtcioWakeupSource` as deep-sleep wake sources.
    pub gpio_btn_refresh: AnyPin<'static>,
    pub gpio_btn_previous: AnyPin<'static>,
    pub gpio_btn_next: AnyPin<'static>,

    /// Pre-built SPI bus (SCK/MOSI, plus MISO on E1004) and pre-built EPD
    /// driver holding its CS/BUSY/DC/RST (and EN, on E1004) pins. The
    /// driver implements [`crate::panel::Panel`] including `enable` /
    /// `disable` for the board-level TFT rail on devices that have one.
    pub spi_bus: Spi<'static, esp_hal::Async>,
    pub epd: P,

    /// Buzzer driver pre-built around the GPIO45 piezo + LEDC peripheral.
    /// Always populated — both E1002 and E1004 have the piezo on the
    /// same pin per Seeed's E10xx ESPHome reference.
    pub buzzer: Buzzer,
}

/// Initialize runtime services that are independent of board wiring.
pub fn init_runtime(
    psram: esp_hal::peripherals::PSRAM<'static>,
    timg0: esp_hal::peripherals::TIMG0<'static>,
    sw_interrupt: esp_hal::peripherals::SW_INTERRUPT<'static>,
) {
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    esp_alloc::psram_allocator!(
        psram,
        esp_hal::psram,
        esp_hal::psram::PsramConfig {
            mode: esp_hal::psram::PsramMode::OctalSpi,
            ..Default::default()
        }
    );

    let timg0 = TimerGroup::new(timg0);
    let sw_ints = esp_hal::interrupt::software::SoftwareInterruptControl::new(sw_interrupt);
    esp_rtos::start(timg0.timer0, sw_ints.software_interrupt0);
}

/// Run the generic app logic after board-specific hardware construction.
pub async fn run_app<P>(spawner: Spawner, board: AppHardware<P>) -> !
where
    P: Panel<Spi<'static, esp_hal::Async>>,
{
    let AppHardware {
        gpio_btn_refresh,
        gpio_btn_previous,
        gpio_btn_next,
        led_pin,
        refresh_button_label,
        has_sy6974b,
        spi_bus,
        epd,
        i2c0,
        flash,
        lpwr,
        wifi,
        battery_enable_pin,
        adc1,
        battery_sense,
        ledc,
        buzzer_pin,
    } = board;

    panic_mode::set_panic_led(led_pin.number());

    let reset_reason = esp_hal::rtc_cntl::reset_reason(esp_hal::system::Cpu::ProCpu);
    let wake_reason = esp_hal::rtc_cntl::wakeup_cause();

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

    let rtc = esp_hal::rtc_cntl::Rtc::new(lpwr);
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
    let config = Config::new(flash).expect("NVS init failed — check partition table and flash");
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

    // Unified app context handed off to whichever top-level flow wins
    // (`panic_mode::run`, `run_normal_boot`, then `normal_mode::run`
    // or `config_mode::run`). The panel's board-level enable rail
    // (E1004's TFT_EN; no-op on E1002 / E1001) stays *off* here —
    // each flow asserts it (idempotently) before its own first panel
    // I/O, so the rail is only powered when the panel is about to be
    // touched.
    let hw = AppContext {
        spawner,
        rtc,
        wake_action,
        wifi,
        refresh_button_label,
        has_sy6974b,
        battery_enable: Output::new(battery_enable_pin, Level::Low, OutputConfig::default()),
        adc1,
        battery_sense,
        i2c0,
        gpio_btn_refresh,
        gpio_btn_previous,
        gpio_btn_next,
        spi_bus,
        epd,
        buzzer: Buzzer::new(ledc, buzzer_pin),
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
    // `run_app()` so the divergence is structural — once we go into
    // `panic_mode::run` we never come back.
    if let Some(panic_msg) = panic_mode::take_pending_message() {
        panic_mode::run(hw, panic_msg.as_str()).await
    } else {
        run_normal_boot(hw, config).await
    }
}

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
        if let Some(action) = crate::normal_mode::PENDING_ACTION.take() {
            println!(
                "Resuming with pending action {:?} from previous cycle",
                action
            );
            return action;
        }
    } else {
        crate::normal_mode::PENDING_ACTION.clear();
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

/// The non-panic boot path: flip the panic-reboot guard on, kick off
/// the white pre-flash, run the config-mode hold race, and dispatch
/// to either `normal_mode::run` (we have credentials and the user isn't
/// holding both Previous + Next) or `config_mode::run`.
async fn run_normal_boot<P>(mut hw: AppContext<P>, config: Config<'static>) -> !
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
        crate::normal_mode::run(hw, config).await
    } else {
        // Entering config mode is a deliberate reset of the device's
        // "what's displayed" state — whatever URL was committed last
        // cycle (and any pending redirect) is no longer relevant once
        // the user has reconfigured.
        crate::normal_mode::CURRENT_URL.clear();
        crate::normal_mode::REDIRECT_URL.clear();
        config_mode::run(hw, config).await
    }
}
