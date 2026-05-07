Power profile — E1004
=====================

Bench measurements taken with a Nordic Power Profiler Kit II in
source-meter mode wired to the E1004 BAT pads (replacing the cell)
at 3700 mV.

Headline: deep-sleep floor went from **1.585 mA → ~250 µA** (≈6×) by
clearing one register before `rtc.sleep_deep(...)`.

Root cause
----------

[esp-rs/esp-hal#2740](https://github.com/esp-rs/esp-hal/issues/2740):
`Adc::new` sets `SENS.sar_power_xpd_sar.force_xpd_sar` and nothing in
the API ever clears it, so the SAR analog stays powered through deep
sleep. ~1.3 mA constant draw on ESP32-S3 once any code calls
`Adc::new` — we hit this every cycle through `read_battery`.

esp-hal #5279 (every `WakeSource::apply()` calls
`set_rtc_peri_pd_en(false)`) was initially suspected of being a
co-contributor; bench bisection showed it isn't material on this
hardware. Clearing the SAR XPD bit alone is sufficient to reach the
~250 µA floor.

Fix in `src/sleep.rs`, immediately before sleep:

```rust
SENS::regs()
    .sar_power_xpd_sar()
    .modify(|_, w| unsafe { w.force_xpd_sar().bits(0) });
```

Power budget
------------

Cell: **5000 mAh × 3.7 V = 18,500 mWh** nominal.

Per active refresh cycle (cold-boot or button-wake, equivalent):
**~36 J ≈ 10 mWh**, ~42 s wall-clock end to end.

| Refresh interval  | Active (mWh/day) | Sleep (mWh/day) | Total (mWh/day) | Battery life     |
|-------------------|------------------|-----------------|-----------------|------------------|
| **24 h (1/day)**  | 10               | 22              | **32**          | **~575 days (~19 months)** |
| 12 h (2/day)      | 20               | 22              | 42              | ~440 days (~14 months)     |
|  6 h (4/day)      | 40               | 22              | 62              | ~300 days (~10 months)     |
|  1 h (24/day)     | 240              | 22              | 262             | ~70 days (~2.3 months)     |

Sleep math: 250 µA × 3.7 V × 24 h ≈ 22 mWh/day.

At a daily refresh, sleep is ~70% of daily energy, active ~30%.
Without the fix, sleep would have been ~140 mWh/day and the same
1-refresh-per-day cell life would be ~120 days (~4 months) — the fix
extends life ~4.7× at this duty cycle.

Active cycle waveform
---------------------

| Phase              | Time         | Mean current | Notes                                     |
|--------------------|--------------|--------------|-------------------------------------------|
| Boot ramp          | 0 – 2 s      | ~70 → 180 mA | ESP32-S3 + radio bring-up; 1.08 A peak    |
| WiFi assoc + fetch | 2 – 12 s     | ~250 mA      | sustained, with sub-second 0.5 A bursts   |
| Panel transition   | 12 – 41 s    | ~180–330 mA  | matches the panel's ~20 s e-ink refresh   |
| Deep sleep         | 42 s onward  | ~250 µA      |                                           |

Active-phase optimisations — considered, parked
------------------------------------------------

Once the deep-sleep floor was at the ESP32-S3 hard floor (~250 µA),
sleep started dominating the daily budget at the typical 1/day refresh
— **22 of 32 mWh/day, ~70%**. That sets a hard ceiling on what
active-phase tuning can achieve at this duty cycle:

> Even reducing the entire active phase to zero only extends battery
> life from ~575 → ~833 days. **Anything we do to active is at most a
> ~45% gain at 1/day.** Realistic combinations top out around +5–10%.

Two candidate optimisations were investigated; neither shipped.

### White pre-flash → buzzer beep (measured)

The white pre-flash gives ~1 s of immediate visual feedback after a
button press by kicking off an all-white panel refresh in parallel
with WiFi fetch. PPK2 measurement of three runs (60 s cold-boot
captures, `disable_charger` build, 3700 mV):

| Variant | Active-phase energy |
|---------|---------------------|
| Stock (with pre-flash), run 1   | 9.413 mWh |
| Stock (with pre-flash), run 2   | 9.155 mWh |
| Pre-flash skipped (no feedback) | 8.836 mWh |
| Buzzer beep instead (80 ms)     | 8.870 mWh |

Pre-flash costs **~0.4 mWh per cycle (~4–5%)** — mostly from the panel
HV booster running concurrently with WiFi for ~6 s, plus a ~1.4 s
longer wall-clock cycle. The buzzer alternative (already on hardware
at GPIO45 via LEDC, used by config_mode) costs ~0.04 mWh — within
run-to-run noise of the no-feedback case. Net saving from swapping
would be the full ~0.4 mWh.

At 1/day on the 5000 mAh cell that's **~595 vs ~587 days (+8 days,
+1.4%)**. Not worth the UX trade (silent visual cue → audible chirp)
for that gain.

### Network phase (theoretical)

WiFi assoc + DHCP + HTTP fetch is t=2–10 s in the cycle waveform,
~250 mA mean → **~2.05 mWh per cycle (~22% of active energy)**. The
levers, in rough order of plausible upside (un-benched estimates):

| Lever | Saved/cycle | Notes |
|-------|-------------|-------|
| Halve image size (server-side PNG re-encode)         | ~0.4 mWh | 547 KB at 4-color indexed is ~2.3 bpp vs ~2 bpp floor. |
| Fast WiFi reconnect (cached BSSID + channel in NVS)  | ~0.25–0.5 mWh | Skips full scan on subsequent associations. |
| Static IP (skip DHCP)                                | ~0.26 mWh | Brittle — LAN reorg breaks the device until reflashed. |
| HTTP `If-Modified-Since` / `ETag` → 304              | ~1.5 mWh | Only when the server actually returns 304. |

Combined plausible "all easy wins" ≈ 1.5 mWh/cycle, daily total 31.5
→ 30.0 mWh/day, battery life 587 → 617 days (**+30 days, +5%** at
1/day). Theoretical max with 304 caching layered on top ≈ 3 mWh/cycle,
~+10%.

### Why none of it shipped

A 1.4 mWh/cycle theoretical maximum saving at 1 refresh/day buys
~10% extra battery life on top of an already-580+-day baseline. None
of the levers are free — image-size and 304 caching are server-side
work, static IP adds a maintenance footgun, fast reconnect is hard
to benchmark reliably. Below the ESP32-S3 sleep floor would require
external hardware (e.g. RTC-controlled rail cut).

**Fast WiFi reconnect** is the one item kept in mind — if a cheap
implementation surfaces (esp-radio knob, NVS-cached BSSID/channel),
it's worth picking up. Bench it with 5+ cycles per variant against
a stable AP; single-cycle deltas are below run-to-run noise.

Bench setup notes
-----------------

PPK2 in source-meter mode replaces the cell at the BAT pads. Build
with the `disable_charger` Cargo feature, which parks the SY6974B in
HIZ so the PPK2 sees the real load even with USB-C attached, and
disables the chip's I²C watchdog so HIZ is sticky across resets:

```
EXTRA_FEATURES=disable_charger RELEASE=1 ./run.sh e1004 /dev/ttyUSB0 /tmp/flash.log
```

Drive the PPK2 from Nordic's nRF Connect for Desktop (Power
Profiler app) for live current display and capture.

**Bench gotcha:** with `disable_charger` active, EN_HIZ is sticky
across ESP32 resets and only clears when VBUS is removed and
reapplied. Once firmware has run, the device only boots if the PPK2
is sourcing on BAT — if the supply script exits between flashes, the
next reset brownouts. Keep one PPK2-supply script running for the
whole bench session.
