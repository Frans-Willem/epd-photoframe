//! Shared hardware container handed between the device-specific binary and
//! whichever mode runs this boot (config vs. normal). The binary chooses the
//! concrete panel type; the common firmware path only requires that it
//! implements [`crate::panel::Panel`].

use embassy_executor::Spawner;
use esp_hal::gpio::AnyPin;
use esp_hal::spi::master::Spi;

use crate::buzzer::Buzzer;

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

/// Hardware and one-shot peripherals handed from the device-specific binary
/// into the common app startup path.
pub struct AppHardware<P> {
    pub spawner: Spawner,
    pub reset_reason: Option<esp_hal::rtc_cntl::SocResetReason>,
    pub wake_reason: esp_hal::system::SleepSource,
    pub wifi: esp_hal::peripherals::WIFI<'static>,
    pub flash: esp_hal::peripherals::FLASH<'static>,
    pub psram: esp_hal::peripherals::PSRAM<'static>,
    pub lpwr: esp_hal::peripherals::LPWR<'static>,
    pub timg0: esp_hal::peripherals::TIMG0<'static>,
    pub sw_interrupt: esp_hal::peripherals::SW_INTERRUPT<'static>,
    pub status_led: AnyPin<'static>,
    pub refresh_button_label: &'static str,
    pub has_sy6974b: bool,

    pub i2c0: esp_hal::peripherals::I2C0<'static>,
    pub gpio_i2c_sda: esp_hal::peripherals::GPIO19<'static>,
    pub gpio_i2c_scl: esp_hal::peripherals::GPIO20<'static>,
    pub adc1: esp_hal::peripherals::ADC1<'static>,
    pub gpio_battery_enable: esp_hal::peripherals::GPIO21<'static>,
    pub gpio_battery_sense: esp_hal::peripherals::GPIO1<'static>,

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

/// Everything both `config_mode::run` and `normal_mode::run` need. The panel
/// driver is pre-built by the selected binary, so the shared modes do not need
/// cfg gates for pin counts or panel-model-specific construction.
pub struct HardwareCtx<P> {
    pub spawner: Spawner,
    pub rtc: esp_hal::rtc_cntl::Rtc<'static>,
    pub wake_action: WakeAction,
    pub wifi: esp_hal::peripherals::WIFI<'static>,
    pub refresh_button_label: &'static str,
    pub has_sy6974b: bool,

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
