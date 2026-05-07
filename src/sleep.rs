use crate::uart;
use embassy_time::Instant;
use esp_hal::peripherals::SENS;
use esp_hal::rtc_cntl;
use esp_println::println;
use heapless::vec::Vec;

pub fn start_sleep<'a>(
    mut rtc: rtc_cntl::Rtc<'a>,
    wakeup_requested: Option<Instant>,
    wakeup_pins: &mut [(&mut dyn esp_hal::gpio::RtcPin, rtc_cntl::sleep::WakeupLevel)],
) -> ! {
    // Shut down ADC fully, see issue https://github.com/esp-rs/esp-hal/issues/2740
    // Saves ~1.3 mA in deep sleep on ESP32-S3 (PPK2-measured on E1004).
    SENS::regs()
        .sar_power_xpd_sar()
        .modify(|_, w| unsafe { w.force_xpd_sar().bits(0) });

    let pin_wake_source = rtc_cntl::sleep::RtcioWakeupSource::new(wakeup_pins);
    let timer_wake_source = wakeup_requested.map(|wakeup_requested| {
        let remaining_time = wakeup_requested.saturating_duration_since(Instant::now());
        // Need to convert embassy_time::Duration to core::time::Duration, do it by converting to
        // and from microseconds
        let remaining_time = core::time::Duration::from_micros(remaining_time.as_micros());
        rtc_cntl::sleep::TimerWakeupSource::new(remaining_time)
    });
    let mut wake_sources: Vec<&dyn rtc_cntl::sleep::WakeSource, 2> = Vec::new();
    // TODO: Remove unwraps
    wake_sources
        .push(&pin_wake_source)
        .ok()
        .expect("Unable to add pin wake source");
    if let Some(timer_wake_source) = timer_wake_source.as_ref() {
        wake_sources
            .push(timer_wake_source)
            .ok()
            .expect("Unable to add timer wake source");
    }
    println!("Going to deep sleep");
    uart::wait_for_tx_idle();
    rtc.sleep_deep(wake_sources.as_slice());
}
