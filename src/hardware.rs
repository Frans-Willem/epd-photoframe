//! Shared hardware container handed between `main()` and whichever mode
//! runs this boot (config vs. normal). Keeping it here means neither mode
//! needs to cfg-gate on the panel model or care about pin counts — they
//! see the same `EpdPanel` alias and the same field set.

use alloc::string::String;

use embassy_executor::Spawner;
use esp_hal::gpio::{AnyPin, Output};
use esp_hal::spi::master::Spi;

use crate::buzzer::Buzzer;

#[cfg(feature = "e1002")]
use crate::gdep073e01::Gdep073e01;
#[cfg(feature = "e1004")]
use crate::t133a01::T133A01;

/// Fully-specialised type of the built panel driver. Both variants
/// implement the [`crate::panel::Panel`] trait so the rest of the firmware
/// can drive the panel without caring which model it is.
#[cfg(feature = "e1002")]
pub type EpdPanel = Gdep073e01<
    Spi<'static, esp_hal::Async>,
    Output<'static>,
    esp_hal::gpio::Input<'static>,
    Output<'static>,
    Output<'static>,
>;
#[cfg(feature = "e1004")]
pub type EpdPanel = T133A01<
    Spi<'static, esp_hal::Async>,
    Output<'static>, // cs_master
    Output<'static>, // cs_slave
    esp_hal::gpio::Input<'static>,
    Output<'static>, // dc
    Output<'static>, // rst
    Output<'static>, // en (TFT_EN rail)
>;

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

/// Everything both `config_mode::run` and `main_normal` need. The panel
/// driver is pre-built (aliased as `EpdPanel` above) so neither mode needs
/// cfg gates for pin counts or panel-model-specific types.
pub struct HardwareCtx {
    pub spawner: Spawner,
    pub rtc: esp_hal::rtc_cntl::Rtc<'static>,
    pub wake_action: WakeAction,
    pub wifi: esp_hal::peripherals::WIFI<'static>,

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
    pub epd: EpdPanel,

    /// Buzzer driver pre-built around the GPIO45 piezo + LEDC peripheral.
    /// Always populated — both E1002 and E1004 have the piezo on the
    /// same pin per Seeed's E10xx ESPHome reference.
    pub buzzer: Buzzer,
}

/// Credentials + URL for the normal flow. Deliberately split out of
/// `HardwareCtx` because config mode doesn't need them.
pub struct WifiCredentials {
    pub ssid: String,
    pub password: String,
    pub base_url: String,
}
