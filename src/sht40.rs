//! SHT40 (temperature + humidity) reading on I²C0. The sensor sits at
//! 0x44, behind GPIO19/GPIO20 (SDA/SCL) on all three devices. It's
//! always-powered (a 0 Ω resistor jumpers SYS_3V3 straight to the
//! sensor's VDD), so there's no enable pin to toggle.

use embassy_time::Delay;
use embedded_hal_async::i2c::I2c;
use esp_println::println;
use fixed::types::I16F16;
use sht4x::{Precision, Sht4xAsync};

/// One temp + humidity measurement. Both as `I16F16` (signed
/// 16.16 fixed point) — the same type the `sht4x` crate uses
/// internally, so no further conversion is needed.
#[derive(Debug, Clone, Copy)]
pub struct TempHumidity {
    pub temperature_c: I16F16,
    pub humidity_pct: I16F16,
}

/// Take one high-precision measurement, returning `None` if the I²C
/// transaction fails (sensor unplugged, bus held by something else,
/// CRC mismatch, …). Borrows the bus for the duration of the
/// transaction so other devices on the same I²C0 (PCF8563 RTC,
/// SY6974B charger on E1004) can be read sequentially.
pub async fn read_temp_humidity<B: I2c>(i2c: &mut B) -> Option<TempHumidity> {
    let mut sensor = Sht4xAsync::new(i2c);
    let mut delay = Delay;
    match sensor.measure(Precision::High, &mut delay).await {
        Ok(m) => {
            let temperature_c = m.temperature_celsius();
            let humidity_pct = m.humidity_percent();
            println!(
                "SHT40: {:.2} °C, {:.2} % RH",
                temperature_c, humidity_pct
            );
            Some(TempHumidity {
                temperature_c,
                humidity_pct,
            })
        }
        Err(e) => {
            println!("SHT40 read failed: {:?}", e);
            None
        }
    }
}
