//! Common firmware entry after a device-specific binary has built the
//! concrete hardware context.

use embassy_time::{Duration, Instant, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pin, Pull};
use esp_hal::timer::timg::TimerGroup;
use esp_println::println;

use crate::buzzer::Buzzer;
use crate::config::Config;
use crate::config_mode;
use crate::hardware::{HardwareCtx, WakeAction};
use crate::panel::{Panel, PanelColor};
use crate::panic_mode;

#[embassy_executor::task]
async fn blink_task(mut led: Output<'static>) {
    loop {
        led.toggle();
        Timer::after(Duration::from_millis(500)).await;
    }
}

pub fn init() -> esp_hal::peripherals::Peripherals {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    esp_hal::init(config)
}

/// App startup object built by the selected device binary.
pub struct App<P> {
    pub spawner: embassy_executor::Spawner,
    pub reset_reason: Option<esp_hal::rtc_cntl::SocResetReason>,
    pub wake_reason: esp_hal::system::SleepSource,
    pub wifi: esp_hal::peripherals::WIFI<'static>,
    pub flash: esp_hal::peripherals::FLASH<'static>,
    pub psram: esp_hal::peripherals::PSRAM<'static>,
    pub lpwr: esp_hal::peripherals::LPWR<'static>,
    pub timg0: esp_hal::peripherals::TIMG0<'static>,
    pub sw_interrupt: esp_hal::peripherals::SW_INTERRUPT<'static>,
    pub status_led: esp_hal::gpio::AnyPin<'static>,
    pub refresh_button_label: &'static str,
    pub has_sy6974b: bool,

    pub i2c0: esp_hal::peripherals::I2C0<'static>,
    pub gpio_i2c_sda: esp_hal::peripherals::GPIO19<'static>,
    pub gpio_i2c_scl: esp_hal::peripherals::GPIO20<'static>,
    pub adc1: esp_hal::peripherals::ADC1<'static>,
    pub gpio_battery_enable: esp_hal::peripherals::GPIO21<'static>,
    pub gpio_battery_sense: esp_hal::peripherals::GPIO1<'static>,

    pub gpio_btn_refresh: esp_hal::gpio::AnyPin<'static>,
    pub gpio_btn_previous: esp_hal::gpio::AnyPin<'static>,
    pub gpio_btn_next: esp_hal::gpio::AnyPin<'static>,

    pub spi_bus: esp_hal::spi::master::Spi<'static, esp_hal::Async>,
    pub epd: P,
    pub buzzer: Buzzer,
}

impl<P> App<P>
where
    P: Panel<esp_hal::spi::master::Spi<'static, esp_hal::Async>> + 'static,
    P::Color: PanelColor + 'static,
    P::Error: core::fmt::Debug,
    P::InitMode: core::fmt::Debug,
{
    pub async fn run(self) -> ! {
        let button_bits = (1u32 << self.gpio_btn_refresh.number())
            | (1u32 << self.gpio_btn_previous.number())
            | (1u32 << self.gpio_btn_next.number());
        let rtc_gpio_int_mask = read_and_clear_rtc_gpio_wake_status(button_bits);

        let refresh_latched = (rtc_gpio_int_mask & (1u32 << self.gpio_btn_refresh.number())) != 0;
        let previous_latched = (rtc_gpio_int_mask & (1u32 << self.gpio_btn_previous.number())) != 0;
        let next_latched = (rtc_gpio_int_mask & (1u32 << self.gpio_btn_next.number())) != 0;

        let wake_action = determine_wake_action(
            self.reset_reason,
            self.wake_reason,
            refresh_latched,
            previous_latched,
            next_latched,
        );

        let rtc = esp_hal::rtc_cntl::Rtc::new(self.lpwr);
        let time_since_boot = rtc.time_since_power_up();
        println!(
            "Device booting up - reset={:?} wake={:?} action={:?} \
         latched[refresh={} previous={} next={}] \
         uptime={time_since_boot:?}",
            self.reset_reason,
            self.wake_reason,
            wake_action,
            refresh_latched,
            previous_latched,
            next_latched,
        );
        println!(
            "RTC CURRENT_URL: {:?}",
            crate::normal_mode::CURRENT_URL.get().as_deref()
        );
        println!(
            "RTC REDIRECT_URL: {:?}",
            crate::normal_mode::REDIRECT_URL.get().as_deref()
        );

        esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
        esp_alloc::psram_allocator!(
            self.psram,
            esp_hal::psram,
            esp_hal::psram::PsramConfig {
                mode: esp_hal::psram::PsramMode::OctalSpi,
                ..Default::default()
            }
        );

        let config =
            Config::new(self.flash).expect("NVS init failed — check partition table and flash");
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

        let timg0 = TimerGroup::new(self.timg0);
        let sw_ints =
            esp_hal::interrupt::software::SoftwareInterruptControl::new(self.sw_interrupt);
        esp_rtos::start(timg0.timer0, sw_ints.software_interrupt0);

        self.spawner.spawn(
            blink_task(Output::new(
                self.status_led,
                Level::Low,
                OutputConfig::default(),
            ))
            .unwrap(),
        );

        let i2c0 =
            esp_hal::i2c::master::I2c::new(self.i2c0, esp_hal::i2c::master::Config::default())
                .unwrap()
                .with_sda(self.gpio_i2c_sda)
                .with_scl(self.gpio_i2c_scl)
                .into_async();

        #[cfg(feature = "disable_charger")]
        let i2c0 = if self.has_sy6974b {
            crate::sy6974b::enter_measurement_mode(i2c0).await
        } else {
            i2c0
        };

        self.spawner.spawn(
            crate::normal_mode::sensor_task(
                Output::new(
                    self.gpio_battery_enable,
                    Level::Low,
                    OutputConfig::default(),
                ),
                self.adc1,
                self.gpio_battery_sense,
                i2c0,
                self.has_sy6974b,
            )
            .unwrap(),
        );

        let hw = HardwareCtx {
            spawner: self.spawner,
            rtc,
            wake_action,
            wifi: self.wifi,
            refresh_button_label: self.refresh_button_label,
            has_sy6974b: self.has_sy6974b,
            gpio_btn_refresh: self.gpio_btn_refresh,
            gpio_btn_previous: self.gpio_btn_previous,
            gpio_btn_next: self.gpio_btn_next,
            spi_bus: self.spi_bus,
            epd: self.epd,
            buzzer: self.buzzer,
        };

        if let Some(panic_msg) = panic_mode::take_pending_message() {
            panic_mode::run(hw, panic_msg.as_str()).await
        } else {
            run_normal_boot(hw, config).await
        }
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

fn determine_wake_action(
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
