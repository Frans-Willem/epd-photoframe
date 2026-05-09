//! Common firmware entry after a device-specific binary has built the
//! concrete hardware context.

use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_println::println;

use crate::config::Config;
use crate::config_mode;
use crate::hardware::{HardwareCtx, WakeAction};
use crate::panel::{Panel, PanelColor};
use crate::panic_mode;

pub async fn run<P>(hw: HardwareCtx<P>, config: Config<'static>) -> !
where
    P: Panel<esp_hal::spi::master::Spi<'static, esp_hal::Async>> + 'static,
    P::Color: PanelColor + 'static,
    P::Error: core::fmt::Debug,
    P::InitMode: core::fmt::Debug,
{
    if let Some(panic_msg) = panic_mode::take_pending_message() {
        panic_mode::run(hw, panic_msg.as_str()).await
    } else {
        run_normal_boot(hw, config).await
    }
}

/// The non-panic boot path: flip the panic-reboot guard on, kick off
/// the white pre-flash, run the config-mode hold race, and dispatch
/// to either `normal_mode::run` (we have credentials and the user isn't
/// holding both Previous + Next) or `config_mode::run`.
async fn run_normal_boot<P>(mut hw: HardwareCtx<P>, config: Config<'static>) -> !
where
    P: Panel<esp_hal::spi::master::Spi<'static, esp_hal::Async>> + 'static,
    P::Color: PanelColor + 'static,
    P::Error: core::fmt::Debug,
    P::InitMode: core::fmt::Debug,
{
    panic_mode::allow_reboot();

    if hw.wake_action.show_white_flash() {
        println!("White pre-flash");
        hw.epd.enable().await.unwrap();
        println!("Reset");
        hw.epd.reset().await.unwrap();
        println!("Wait until idle");
        hw.epd.wait_until_idle().await.unwrap();
        println!("Init");
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
        crate::normal_mode::CURRENT_URL.clear();
        crate::normal_mode::REDIRECT_URL.clear();
        config_mode::run(hw, config).await
    }
}

pub fn determine_wake_action(
    reset_reason: Option<esp_hal::rtc_cntl::SocResetReason>,
    wake_reason: esp_hal::system::SleepSource,
    refresh_latched: bool,
    previous_latched: bool,
    next_latched: bool,
) -> WakeAction {
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
        esp_hal::system::SleepSource::Undefined => WakeAction::FreshBoot,
        esp_hal::system::SleepSource::Timer => WakeAction::Timer,
        esp_hal::system::SleepSource::Gpio => {
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

pub fn read_and_clear_rtc_gpio_wake_status(mask: u32) -> u32 {
    let rtc_io = esp_hal::peripherals::RTC_IO::regs();
    let value = rtc_io.rtc_gpio_status().read().int().bits() & mask;
    unsafe {
        rtc_io
            .rtc_gpio_status_w1tc()
            .write(|w| w.rtc_gpio_status_int_w1tc().bits(mask));
    }
    value
}
