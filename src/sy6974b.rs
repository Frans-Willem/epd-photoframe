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
const REG_INPUT_SOURCE: u8 = 0x00;
const REG_CHARGE_TERM: u8 = 0x05;
const REG_STATUS: u8 = 0x08;

/// REG00 bit 7. When set, the charger ignores VBUS and the system rail
/// runs entirely from BAT — USB-C can stay plugged in without feeding
/// the system. Used by [`enter_measurement_mode`].
const REG00_EN_HIZ: u8 = 1 << 7;

/// REG05 bits 5:4 = WATCHDOG[1:0]. Default `01` (40 s); we mask these
/// to `00` (timer disabled) so HIZ doesn't get reset back to default
/// when the timer fires.
const REG05_WATCHDOG_MASK: u8 = 0b0011_0000;

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

/// Park the charger so a Nordic Power Profiler Kit II on the BAT pins
/// sees the real system load even with USB-C still plugged in. Takes
/// the bus by value and returns it so the caller doesn't need a `mut`
/// binding when the cfg-gate behind which this is called is off.
///
/// Two writes, both read-modify-write so other bits keep their POR
/// values:
///
/// 1. **REG05 WATCHDOG → 00 (disable).** Default is 40 s; on expiry
///    the chip "gets back to the default mode" (datasheet § BATFET
///    Control), which would clear EN_HIZ. Disable it first so it can't
///    fire between the two writes.
/// 2. **REG00 EN_HIZ → 1.** Input goes high-impedance, BAT supplies
///    VSYS through the BATFET. CH340K stays alive on the separate
///    USB-VBUS → TPL740F33 LDO rail (`UART_3V3`), independent of the
///    charger, so `esptool` and the serial console keep working.
///
/// Both transactions log; on I²C failure the function returns the bus
/// and the chip stays in whatever state it was in.
pub async fn enter_measurement_mode<B: I2c>(mut i2c: B) -> B {
    let mut reg05 = [0u8; 1];
    if let Err(e) = i2c
        .write_read(I2C_ADDR, &[REG_CHARGE_TERM], &mut reg05)
        .await
    {
        println!("SY6974B REG05 read failed: {:?}", e);
        return i2c;
    }
    let new_reg05 = reg05[0] & !REG05_WATCHDOG_MASK;
    if let Err(e) = i2c.write(I2C_ADDR, &[REG_CHARGE_TERM, new_reg05]).await {
        println!("SY6974B REG05 write failed: {:?}", e);
        return i2c;
    }
    println!(
        "SY6974B watchdog disabled: REG05 0x{:02x} -> 0x{:02x}",
        reg05[0], new_reg05
    );

    let mut reg00 = [0u8; 1];
    if let Err(e) = i2c
        .write_read(I2C_ADDR, &[REG_INPUT_SOURCE], &mut reg00)
        .await
    {
        println!("SY6974B REG00 read failed: {:?}", e);
        return i2c;
    }
    let new_reg00 = reg00[0] | REG00_EN_HIZ;
    if let Err(e) = i2c.write(I2C_ADDR, &[REG_INPUT_SOURCE, new_reg00]).await {
        println!("SY6974B REG00 write failed: {:?}", e);
        return i2c;
    }
    println!(
        "SY6974B HIZ enabled: REG00 0x{:02x} -> 0x{:02x} (system on BAT only)",
        reg00[0], new_reg00
    );
    i2c
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
