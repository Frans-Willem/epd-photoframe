//! Async helpers for button input.

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use esp_hal::gpio::Input;

/// Wait for an active-low button to be held for at least `hold_duration`.
/// Electrical / contact-bounce blips that release before the duration
/// elapses are filtered out (the `wait_for_high` race wins and we loop
/// back to `wait_for_low`). Useful when we've just enabled a pull-up
/// and the rails are still settling, or for general debouncing of a
/// momentary press.
pub async fn wait_for_press(input: &mut Input<'_>, hold_duration: Duration) {
    loop {
        input.wait_for_low().await;
        match select(Timer::after(hold_duration), input.wait_for_high()).await {
            Either::First(()) => return,
            Either::Second(()) => continue,
        }
    }
}
