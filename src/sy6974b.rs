//! Silergy SY6974B Li-ion charger reader on I²C0 (E1004 only).
//!
//! Verified bit layout (Silergy doesn't publish a public datasheet,
//! so this is from a register dump cross-referenced against TI's
//! BQ-family docs): I²C address 0x6B, status register **0x08**,
//! bit fields VBUS_STAT (7..=5), CHRG_STAT (4..=3), PG_STAT (2). The
//! chip is a hybrid — status-register *location* matches BQ24296
//! (REG08), but the *bit layout* matches BQ25895 (3-bit VBUS instead
//! of BQ24296's 2-bit). REG0B looks like a part-revision register.

use embedded_hal_async::i2c::I2c;
use esp_println::println;

const I2C_ADDR: u8 = 0x6B;
const REG_STATUS: u8 = 0x08;

/// What the charger thinks the system is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerStatus {
    /// No USB attached, running off the cell.
    Battery,
    /// USB attached and the cell is being charged (pre-charge or
    /// fast-charge — both look the same to the user).
    Charging,
    /// USB attached and charging has completed (cell at float).
    Full,
    /// USB present but PG_STAT not asserted, or some other unexpected
    /// combination — typically a transient during plug events, but
    /// also covers genuine faults.
    Fault,
}

impl PowerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PowerStatus::Battery => "battery",
            PowerStatus::Charging => "charging",
            PowerStatus::Full => "full",
            PowerStatus::Fault => "fault",
        }
    }
}

/// Read the status register, decode, and return the high-level
/// state. `None` if the I²C transaction itself failed.
pub async fn read_power_status<B: I2c>(i2c: &mut B) -> Option<PowerStatus> {
    let mut buf = [0u8; 1];
    if let Err(e) = i2c.write_read(I2C_ADDR, &[REG_STATUS], &mut buf).await {
        println!("SY6974B read failed: {:?}", e);
        return None;
    }
    let reg = buf[0];
    let vbus_stat = (reg >> 5) & 0b111;
    let chrg_stat = (reg >> 3) & 0b11;
    let pg_stat = (reg >> 2) & 0b1;

    let status = if vbus_stat == 0b000 {
        PowerStatus::Battery
    } else if pg_stat == 0 {
        PowerStatus::Fault
    } else {
        match chrg_stat {
            // Pre-charge (low cell V) and fast-charge both read as
            // "charging" to the user.
            0b01 | 0b10 => PowerStatus::Charging,
            0b11 => PowerStatus::Full,
            _ => PowerStatus::Fault,
        }
    };
    println!(
        "SY6974B REG08=0x{:02x} (vbus={:03b} chrg={:02b} pg={}) -> {}",
        reg,
        vbus_stat,
        chrg_stat,
        pg_stat,
        status.as_str()
    );
    Some(status)
}
