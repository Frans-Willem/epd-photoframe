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

/// Run the full enable → settle → sample → disable sequence and
/// return battery voltage in millivolts.
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

    // The curve calibration scheme returns mV directly; the ÷2 divider
    // on the board halves V_BAT into the ADC, so multiply by 2 to
    // recover battery voltage.
    let sense_mv = adc.read_blocking(&mut adc_pin);
    let battery_mv = sense_mv.saturating_mul(2);

    enable_pin.set_low();

    println!(
        "Battery: {} mV ({}%)",
        battery_mv,
        mv_to_percentage(battery_mv)
    );
    battery_mv
}
