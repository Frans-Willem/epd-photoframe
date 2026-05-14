Power profiles
==============

Practical power budget for the supported reTerminal E10xx devices.
Measurements were taken with a Nordic Power Profiler Kit / PPK2 at
**3.7 V** supply voltage.

Device summary
--------------

| Device | Battery assumption | Sleep current | Sleep energy/day | Refresh cycle | Active energy/cycle |
|--------|--------------------|---------------|------------------|---------------|---------------------|
| E1001 | 3.7 V, 2000 mAh = 7,400 mWh | **305 µA** | **27.08 mWh/day** | ~143 mA for ~6.75 s | **0.99 mWh** |
| E1002 | 3.7 V, 2000 mAh = 7,400 mWh | **297.93 µA** | **26.46 mWh/day** | ~122.85 mA for ~24.14 s | **3.05 mWh** |
| E1004 | 3.7 V, 5000 mAh = 18,500 mWh | **250.32 µA** | **22.23 mWh/day** | ~255.68 mA for ~37.11 s | **9.76 mWh** |

Sleep dominates normal daily use. At one refresh per day, active
energy is only ~4% of E1001's budget, ~10% of E1002's budget, and
~31% of E1004's budget.

E1001 / E1002 still sleep about **48-55 µA** higher than E1004. We did
not find a firmware cause after checking ADC shutdown, panel reset
state, and panel-controller sleep. The remaining difference is
suspected to be hardware-related: board pull networks, panel/FPC
leakage, charger/power-path quiescent current, or regulator differences.

Expected battery life
---------------------

| Wake-up interval | 24 h | 12 h | 6 h | 1 h |
|------------------|------|------|-----|-----|
| E1001 | **~264 days (~8.7 months)** | ~255 days (~8.4 months) | ~238 days (~7.8 months) | ~145 days (~4.8 months) |
| E1002 | **~251 days (~8.2 months)** | ~227 days (~7.5 months) | ~192 days (~6.3 months) | ~74 days (~2.4 months) |
| E1004 | **~578 days (~19 months)** | ~443 days (~14.6 months) | ~302 days (~9.9 months) | ~72 days (~2.4 months) |

Sleep-current fixes
-------------------

### ESP32-S3 ADC shutdown

[esp-rs/esp-hal#2740](https://github.com/esp-rs/esp-hal/issues/2740):
`Adc::new` sets `SENS.sar_power_xpd_sar.force_xpd_sar` and nothing in
the API ever clears it, so the SAR analog stays powered through deep
sleep. That costs ~1.3 mA on ESP32-S3 once any code calls `Adc::new`;
we hit this every cycle through `read_battery`.

Fix in `src/sleep.rs`, immediately before sleep:

```rust
SENS::regs()
    .sar_power_xpd_sar()
    .modify(|_, w| unsafe { w.force_xpd_sar().bits(0) });
```

This brought E1004 deep sleep from **1.585 mA** down to **250.32 µA**.
[esp-rs/esp-hal#5279](https://github.com/esp-rs/esp-hal/issues/5279)
(`WakeSource::apply()` calling
`set_rtc_peri_pd_en(false)`) was initially suspected, but bench
bisection showed it is not material on this hardware.

### E1001 panel rail shutdown

E1001 originally slept at **394.67 µA**, higher than E1002's
**297.93 µA**. Adding the UC8179 `DeepSleep(0xA5)` command after panel
`PowerOff` only moved E1001 to **~374 µA**, so controller standby was
not the main source.

The material fix is holding `SCREEN_RST#` low in the GDEY075T7
`disable()` path. On E1001 that reset net also drives the 24-pin panel
load-switch enable, so pulling it low drops the panel rail before ESP
deep sleep. That accounts for the E1001-specific **~90 µA** sleep
excess. The UC8179 deep-sleep command is kept anyway as the correct
controller shutdown sequence before the rail is removed.

Measurement notes
-----------------

Measured at 3.7 V with USB disconnected. PPK / PPK2 in source-meter
mode replaces the cell at the BAT pads.

| Device | Mode | Mean current | Duration | Energy |
|--------|------|--------------|----------|--------|
| E1001 | Sleep, before reset/rail fix | 394.67 µA | - | 35.05 mWh/day |
| E1001 | Sleep, after reset/rail fix | 305 µA | - | 27.08 mWh/day |
| E1001 | Wake-refresh 1 | 146.79 mA | 6.939 s | 1.05 mWh |
| E1001 | Wake-refresh 2 | 145.35 mA | 5.942 s | 0.89 mWh |
| E1001 | Button-refresh 1 | 140.32 mA | 7.038 s | 1.02 mWh |
| E1001 | Button-refresh 2 | 140.08 mA | 7.067 s | 1.02 mWh |
| E1002 | Sleep | 297.93 µA | - | 26.46 mWh/day |
| E1002 | Wake-refresh 1 | 123.62 mA | 24.23 s | 3.08 mWh |
| E1002 | Wake-refresh 2 | 122.06 mA | 23.20 s | 2.91 mWh |
| E1002 | Button-refresh 1 | 122.93 mA | 24.62 s | 3.11 mWh |
| E1002 | Button-refresh 2 | 122.78 mA | 24.51 s | 3.09 mWh |
| E1004 | Sleep | 250.32 µA | - | 22.23 mWh/day |
| E1004 | Wake-refresh 1 | 249.73 mA | 36.08 s | 9.26 mWh |
| E1004 | Wake-refresh 2 | 250.44 mA | 35.84 s | 9.23 mWh |
| E1004 | Button-refresh 1 | 264.46 mA | 38.37 s | 10.43 mWh |
| E1004 | Button-refresh 2 | 258.08 mA | 38.15 s | 10.12 mWh |

Active refresh averages **~255.68 mA for ~37.11 s**, or
**~35.13 J ≈ 9.76 mWh** on E1004.

Build with the `power_measurement` Cargo feature for bench captures.
It parks the SY6974B in HIZ on E1004, forces a 60-second wake interval,
and marks the panel image so bench firmware is obvious. HIZ lets you
measure E1004 power at the battery connector while USB serial remains
attached; on E1001 / E1002, disconnect USB for power measurements.

```
EXTRA_FEATURES=power_measurement RELEASE=1 ./run.sh e1004 /dev/ttyUSB0 /tmp/flash.log
```

Active-phase optimisations
--------------------------

Once the deep-sleep floor is near the ESP32-S3 hard floor, sleep
dominates the daily budget at the typical 1/day refresh. That limits
what active-phase tuning can achieve at this duty cycle.

For E1004, even reducing the entire active phase to zero only extends
battery life from ~578 to ~832 days. Realistic active-phase changes
are much smaller.

### White pre-flash -> buzzer beep

On E1004, the white pre-flash gives ~1 s of immediate visual feedback
after a button press by kicking off an all-white panel refresh in
parallel with WiFi fetch. PPK2 measurement of three runs showed:

| Variant | Active-phase energy |
|---------|---------------------|
| Stock with pre-flash, run 1 | 9.413 mWh |
| Stock with pre-flash, run 2 | 9.155 mWh |
| Pre-flash skipped | 8.836 mWh |
| Buzzer beep instead, 80 ms | 8.870 mWh |

Pre-flash costs **~0.4 mWh per cycle (~4-5%)**. At 1/day on the
5000 mAh E1004 cell, swapping it for a buzzer beep would be roughly
**+8 days (+1.4%)**, not enough to justify the UX trade.

### Network phase

E1004 WiFi assoc + DHCP + HTTP fetch is roughly t=2-10 s in the cycle
waveform, ~250 mA mean, or **~2.05 mWh per cycle**. These measurements
already include fast WiFi reconnect using the cached BSSID/channel
hint. Remaining plausible levers:

| Lever | Saved/cycle | Notes |
|-------|-------------|-------|
| Halve image size | ~0.4 mWh | Server-side PNG re-encode; 547 KB at 4-color indexed is ~2.3 bpp vs ~2 bpp floor. |
| Static IP | ~0.26 mWh | Brittle: LAN reorg breaks the device until reflashed. |
| HTTP `If-Modified-Since` / `ETag` -> 304 | ~1.5 mWh | Only when the server actually returns 304. |
