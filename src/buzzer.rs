//! Buzzer driver for the GPIO45 piezo on the reTerminal E10xx series.
//!
//! Sets up LEDC LowSpeed Timer0 + Channel0 once at construction; each
//! `beep` call just flips the channel between 50 % duty (driving the
//! piezo) and 0 % (silent) for the requested duration. Frequency is
//! fixed at construction time — the channel keeps a reference to the
//! timer internally, so reconfiguring the timer between beeps would
//! conflict with that borrow. If multi-pitch tones become a thing
//! later, refactor.

use embassy_time::{Duration, Timer};
use esp_hal::gpio::DriveMode;
use esp_hal::gpio::interconnect::PeripheralOutput;
use esp_hal::ledc::channel::{self, Channel, ChannelIFace};
use esp_hal::ledc::timer::{self, LSClockSource, Timer as LedcTimer, TimerIFace};
use esp_hal::ledc::{LSGlobalClkSource, Ledc, LowSpeed};
use esp_hal::peripherals::LEDC;
use esp_hal::time::Rate;
use static_cell::StaticCell;

/// Tone pitch — ~2 kHz lands in the most-audible band of a small
/// piezo without being shrill.
const BUZZER_FREQ: Rate = Rate::from_hz(2000);

pub struct Buzzer {
    channel: Channel<'static, LowSpeed>,
    // Held to keep the LEDC peripheral claimed for the lifetime of
    // the Buzzer; not used directly after construction.
    _ledc: Ledc<'static>,
}

impl Buzzer {
    /// Claim the LEDC peripheral and `output_pin`, configure them as a
    /// fixed-frequency PWM source, and leave the channel sitting at
    /// 0 % duty (silent). `Buzzer::new` may only be called once per
    /// boot — the timer is leaked into a `StaticCell` so the channel
    /// can hold a 'static reference to it.
    pub fn new(ledc_peripheral: LEDC<'static>, output_pin: impl PeripheralOutput<'static>) -> Self {
        static PWM_TIMER: StaticCell<LedcTimer<'static, LowSpeed>> = StaticCell::new();

        let mut ledc = Ledc::new(ledc_peripheral);
        ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);

        let pwm_timer = PWM_TIMER.init({
            let mut t = ledc.timer::<LowSpeed>(timer::Number::Timer0);
            // Duty resolution has to be high enough that
            //   divisor = (APB << 8) / freq / 2^duty_bits  ≤  0x3FFFF.
            // At APB = 80 MHz, freq = 2 kHz, 5 bits gives 320 000 (over),
            // 10 bits gives 10 000 (well within range).
            t.configure(timer::config::Config {
                duty: timer::config::Duty::Duty10Bit,
                clock_source: LSClockSource::APBClk,
                frequency: BUZZER_FREQ,
            })
            .unwrap();
            t
        });

        let mut tone_channel = ledc.channel::<LowSpeed>(channel::Number::Channel0, output_pin);
        tone_channel
            .configure(channel::config::Config {
                timer: pwm_timer,
                duty_pct: 0,
                drive_mode: DriveMode::PushPull,
            })
            .unwrap();

        Buzzer {
            channel: tone_channel,
            _ledc: ledc,
        }
    }

    /// Drive the piezo for `duration`, then go silent.
    pub async fn beep(&mut self, duration: Duration) {
        self.channel.set_duty(50).unwrap();
        Timer::after(duration).await;
        self.channel.set_duty(0).unwrap();
    }
}
