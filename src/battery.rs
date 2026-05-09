//! Battery state-of-charge for the reTerminal E10xx series.
//!
//! Hardware (per Seeed's E10xx ESPHome reference):
//! - GPIO1  — ADC1_CH0, signal is V_BAT / 2 (on-board divider).
//! - GPIO21 — Enable for the divider's high side; must be high for at
//!   least ~10 ms before the ADC sample for the rail to settle.

use embassy_time::{Duration, Timer};
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation};
use esp_hal::gpio::Output;
use esp_hal::peripherals::{ADC1, GPIO1};
use esp_println::println;

/// Convert battery voltage in millivolts to a state-of-charge
/// percentage using Seeed's 12-point breakpoint curve (linear
/// interpolation between adjacent points; clamped to 0..=100 outside
/// the table).
///
/// Source: <https://wiki.seeedstudio.com/reterminal_e10xx_with_esphome_advanced/>
/// (the `calibrate_linear` filter on the battery-level template).
pub fn mv_to_percentage(mv: u16) -> u8 {
    const CURVE: &[(u16, u8)] = &[
        (3270, 0),
        (3300, 5),
        (3410, 10),
        (3490, 20),
        (3580, 30),
        (3680, 40),
        (3750, 50),
        (3800, 60),
        (3850, 70),
        (3910, 80),
        (3960, 90),
        (4150, 100),
    ];
    if mv <= CURVE[0].0 {
        return 0;
    }
    if mv >= CURVE[CURVE.len() - 1].0 {
        return 100;
    }
    for window in CURVE.windows(2) {
        let (v0, p0) = window[0];
        let (v1, p1) = window[1];
        if mv <= v1 {
            let dx = (mv - v0) as u32;
            let span = (v1 - v0) as u32;
            let dy = (p1 - p0) as u32;
            return (p0 as u32 + dx * dy / span) as u8;
        }
    }
    0 // unreachable — clamped above
}

/// Number of ADC samples taken per `read_battery` call. Odd so the
/// median is unambiguous; large enough to reject a couple of outliers,
/// small enough to keep the overall sampling window short (≈ 8 ms).
const SAMPLE_COUNT: usize = 9;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(1);

/// Run the full enable → settle → sample-window → disable sequence and
/// return battery voltage in millivolts. Takes nine ADC samples ~1 ms
/// apart and returns the median, which rejects the occasional outlier
/// that running concurrently with WiFi association can introduce.
pub async fn read_battery(
    mut enable_pin: Output<'static>,
    adc_peripheral: ADC1<'static>,
    sense_pin: GPIO1<'static>,
) -> u16 {
    enable_pin.set_high();
    Timer::after(Duration::from_millis(10)).await;

    let mut adc_config = AdcConfig::<ADC1<'static>>::new();
    let mut adc_pin = adc_config
        .enable_pin_with_cal::<_, AdcCalCurve<ADC1<'static>>>(sense_pin, Attenuation::_11dB);
    let mut adc = Adc::new(adc_peripheral, adc_config);

    let mut samples = [0u16; SAMPLE_COUNT];
    for (i, sample) in samples.iter_mut().enumerate() {
        if i > 0 {
            Timer::after(SAMPLE_INTERVAL).await;
        }
        *sample = adc.read_blocking(&mut adc_pin);
    }
    enable_pin.set_low();

    samples.sort_unstable();
    // The curve calibration scheme returns mV directly; the ÷2 divider
    // on the board halves V_BAT into the ADC, so multiply by 2 to
    // recover battery voltage.
    let battery_mv = samples[SAMPLE_COUNT / 2].saturating_mul(2);

    println!(
        "Battery: {} mV ({}%)",
        battery_mv,
        mv_to_percentage(battery_mv)
    );
    battery_mv
}
