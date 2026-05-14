# TODO

## Possible TRMNL compatibility

TRMNL compatibility might be useful eventually, but it is not a current
goal and may never be implemented. If we do pick it up, decide first
whether compatibility means speaking a TRMNL-compatible HTTP endpoint,
matching TRMNL button semantics, reusing any panel-waveform behaviour,
or some smaller subset of those.

## Static-IP + explicit WPA auth-type configuration

The current NVS schema stores just the three fields the portal form
surfaces: `wifi.ssid`, `wifi.pass`, `image.url`. Three more fields are
reserved for future expansion — adding them when the user actually
needs them is a portal-form extension plus a small config loader
change, no schema migration.

| Key         | Type | Purpose |
|-------------|------|---------|
| `wifi.auth` | str  | `"open"` or `"wpa2"`. If absent, auto-detect from whether `wifi.pass` is empty. |
| `net.mode`  | str  | `"dhcp"` (default) or `"static"`. |
| `net.addr`  | u32  | Big-endian IPv4 address. Only if `net.mode == "static"`. |
| `net.mask`  | u8   | CIDR prefix length (e.g. 24). Only if static. |
| `net.gw`    | u32  | IPv4 gateway. Only if static. |
| `net.dns1`  | u32  | Optional DNS server. |
| `net.dns2`  | u32  | Optional second DNS server. |

`embassy_net` already supports both via `Config::dhcpv4(..)` and
`Config::ipv4_static(StaticConfigV4 { address, gateway, dns_servers })`
— the switch is just which variant we pass in. The ui change is a
mode-selector (DHCP / static) on the portal form that conditionally
shows the IP / mask / gateway / DNS fields.

## Power saving — further audit

WiFi is now brought up and torn down inside
`single_shot_wifi::run`, so the radio is off for the ~20 s panel
refresh + UART flush that used to run with it on. That also trimmed
the on-time per cycle by ~8 s (lost DHCP backoff was pure wasted
radio time).

Remaining: audit what's *still* powered during the refresh /
post-fetch stretch and shut down anything else we don't need. PSRAM
(octal rail), the panel SPI bus after `power_off`, the I²C0 bus
after `sensor_task` finishes (SHT40 + SY6974B reads), and any other
peripherals that might be left initialised. The sensor reads are
already short-lived (their tasks exit after one measurement), so
the bus should be safe to tear down before the deep-sleep call.

The ~20 s panel-refresh wait already idles the CPU: our
`wait_until_idle` is an interrupt-driven `busy.wait_for_high().await`,
so the task is parked until the GPIO interrupt fires, and esp-rtos's
default idle hook (`esp-rtos-0.3.0/src/task/mod.rs:36`) loops on
`esp_hal::interrupt::wait_for_interrupt()`, which is the core's WFI.
So that window is already "sleeping" as far as the CPU core is
concerned. Deeper sleep modes (light-sleep with RAM retention, or
deep-sleep with RTC-IO wake and resume-on-next-boot) would save
more — worth revisiting once we have battery-life measurements to
justify the added complexity.

## Wire up host-runnable unit tests

`src/grayscale.rs` carries `#[cfg(test)] mod tests` for `Gray2::from_rgb`
(level anchors, boundary rounding, BT.601 weighting). `cargo test` does
not currently run them: the crate is `#![no_std]` with the build target
pinned to `xtensa-esp32s3-none-elf` in `.cargo/config.toml`, the test
harness needs `std`, and forcing `--target $(host)` fails because
`esp-hal` and friends aren't portable.

Two reasonable shapes:

1. **Workspace + sibling test crate**: split out a leaf crate
   `epd-photoframe-core` that holds the portable modules
   (`spectra6`, `grayscale`, `panel`, `iter_util`, …) with no esp-hal
   dependency, then have a sibling `epd-photoframe-core-tests` that
   targets the host. Firmware depends on the core crate as today.
2. **In-tree feature gate**: add a `host-tests` feature that
   `#[cfg]`-gates out every esp-hal-dependent module so the lib can
   compile for `x86_64-unknown-linux-gnu` when that feature is on.
   Cheaper to land but uglier to maintain — every new module needs to
   pick a side.

(1) is the cleaner long-term answer. Start there if we add more than
one or two test files; otherwise (2) buys time.

## E1001 4-gray waveform LUT — validate on hardware

`src/gdey075t7.rs` currently ships 4-level grayscale with the LUT
bytes from GxEPD2_4G v1.0.9. We have no on-hardware confirmation for
that combination yet. Two open uncertainties to resolve when a real
panel is in hand:

**1. LUT length.** UC8179 datasheet (`/tmp/goodisplay/UC8179.pdf`,
Command Description for R20H / R22H / R23H / R24H) documents LUTC,
LUTKW, LUTWK, and LUTKK as 60-byte LUTs (10 groups × 6 bytes). We
write only 42 bytes (7 groups) for those four. GxEPD2_4G is reportedly
production-validated; the chip is presumably zero-padding the missing
groups, but if hardware shows ghosting / banding the first thing to
try is appending three `[0; 6]` groups to those four LUTs.

**2. LUT bytes themselves and bit-mapping convention.** TRMNL firmware
(`usetrmnl/trmnl-firmware` → `bitbank2/bb_epaper@2.1.7`) is
*verified-good* on this panel by the user. Their bytes differ from
ours in non-trivial ways:

- LUT_BB byte 0: ours `0x80` vs TRMNL `0x40`.
- LUT_BD entirely different (4 active phases for TRMNL vs 1 for us).
- Phase-2/3/4 timings and amplitudes diverge meaningfully across the
  six LUTs.
- CDI register `0x50`: ours writes `0x31, 0x07`, TRMNL writes
  `0x00, 0x07`.
- **Bit-mapping convention is inverted**: ours (matching GxEPD2_4G)
  packs the 2-bit gray code so `white = (high=1, low=1)`,
  `black = (0, 0)`. TRMNL's bb_epaper packs the inverse:
  `white = (0, 0)`, `black = (1, 1)`. The two are internally
  consistent with their respective LUT waveforms — **cannot be mixed
  piecemeal**.

PANEL_SETTING (`0x00`) value is `0x3F` in both → matches ours ✓.

Reference points for swap:

- TRMNL `epd75_old_gray_init` (most likely match for plain non-D2
  GDEY075T7) — `bitbank2/bb_epaper@2.1.7` in `src/bb_ep.inl` around
  line 4550. For the GEN2 / GDEY075T7-D2 panel, TRMNL uses
  `epd75_gray_init` which writes no LUTs and triggers OTP via
  `0xE5 0x5F` (PSR `0x1F` + CDI `0x90, 0x07`).
- TRMNL repo: `usetrmnl/trmnl-firmware` master @ commit `40dafef8`.

To do, in order, when a real E1001 lands:

1. Flash with the GxEPD2_4G LUTs as-is. Display a four-band
   grayscale test pattern (Black / DarkGray / LightGray / White
   vertical strips).
2. If the four levels are visually distinct and clean — done.
3. If output is *inverted* (whites are black, etc.) but otherwise
   clean: that's the bit-mapping mismatch. Either flip our
   `pack_plane` encoding *or* swap to TRMNL's LUTs — pick one
   approach and stick with it.
4. If output is recognisable but the gray levels are wrong / dirty:
   try padding the four short LUTs to 60 bytes (option 1).
5. If still wrong, swap LUT bytes + CDI value + bit-mapping
   wholesale to TRMNL's `epd75_old_gray_init` (option 2). Confirm
   visually, then keep whichever set works.

## SY6974B charger reporting on E1001 / E1002 (BLOCKED — chip silent on bus)

The plumbing for this — bringing up I²C1 on GPIO39/40, sharing both
buses via `Arc<Mutex<I2c>>` + `embassy_embedded_hal`'s `I2cDevice`,
running the charger read on all three devices — landed on branch
`experiment/sy6974b-i2c` along with a one-shot bus-scan diagnostic.

On bench testing, the SY6974B does not respond on either E1001 or
E1002 at any address from `0x08..=0x77`, even after dropping I²C1 to
10 kHz. I²C0 sensors (SHT40 `0x44` + PCF8563 RTC `0x51`) respond
fine on the same software path. Pinout (GPIO39 SDA / GPIO40 SCL)
verified against the schematic. ESP32-S3 internal pull-ups are on
by default in esp-hal 1.1.0.

Open hypotheses (in priority order):

1. **Trace continuity.** ESP32 GPIO39/40 → SY6974B SDA/SCL may not
   be a direct connection. Could be a missing 0Ω jumper, unstuffed
   series resistor, analog mux, or level shifter that needs an
   enable. Probe SDA at the chip pad during a scan to see whether
   master-side wiggles reach the IC.
2. **Shipping mode.** BQ-family parts ship with a low-power latch
   that ignores I²C until VBUS is applied directly to the charger's
   input pin or QON is pulsed low for >2 s. If USB on this board
   only powers the ESP32 (via USB-UART) and the charger has its own
   VBUS path that needs separate power, the chip never wakes.
3. **CE / chip-enable held HIGH from boot.** Some BQ variants gate
   I²C off when CE is not asserted. Trace CE on the schematic; if
   it's on a GPIO, drive it low before scanning.
4. **External pull-up strength.** Internal ~45 kΩ pull-ups are
   weak; I²C norm is 4.7-10 kΩ external. 10 kHz scan still found
   nothing, which mostly rules this out, but it could compound
   another issue at higher speeds.

To pick this back up: cherry-pick or rebase `experiment/sy6974b-i2c`,
resolve the hardware question, then drop the diagnostic scan + the
10 kHz I²C1 clock (both flagged with `TEMP:` comments in that
branch).

DTS values to match if we ever write to `REG02` / `REG04`:
charge current 500 mA, charge voltage 4.208 V on E1001 / E1002 (the
E1004 DTS lists 1000 mA — a different battery pack).

## E1004: investigate quad-SPI for panel data

The 60-pin FPC on the E1004 routes four panel-side data lines:
SPLD0..SPLD3 connect to GPIO9 (MOSI / SPLD0), GPIO8 (MISO / SPLD1),
GPIO17 (SPLD2), and GPIO18 (SPLD3) — plus SPLCLK on GPIO7 and
SPLCS_M on GPIO10. The schematic (page 8) shows a BS0 / BS1 strap
table indicating the panel can run in 3-wire SPI, 4-wire SPI, or
dual / quad SPI depending on those resistors.

Today the firmware claims only MOSI + MISO via `Spi::with_mosi` /
`with_miso` and leaves SPLD2 / SPLD3 floating. If the panel's
strapping (the BS0 / BS1 resistor pair near the FPC) selects
quad SPI, we're leaving 2× or 4× of the per-frame SPI write
bandwidth on the table — meaningful on the 1600×1200 panel where
loading a frame buffer is one of the longer steps.

To do:

1. Read the populated values of the BS0 / BS1 resistors on a real
   device (or the BOM if available) to confirm which SPI mode the
   panel is strapped for.
2. If it's wider than single-data-line SPI, switch the SPI bus
   setup in `main.rs` and the `T133A01` driver to esp-hal's
   `with_sio2` / `with_sio3` (or whatever the equivalent quad-SPI
   API is in 1.1.0-rc.0) and use `SpiDataMode::Quad`.
3. Re-tune the SPI clock — quad mode often allows a higher
   frequency too.
4. Measure the frame-load time before and after.

E1002 panel uses a different (50-pin) FPC and is not affected.

## Revisit the white pre-flash on E1004 — does it still earn its keep?

The white pre-flash kicked off in `main.rs` before the config-mode
race resolves exists purely as immediate visual feedback that the
button press / power-on registered. Two things have changed since it
was added:

1. The wake-to-real-refresh window is now ~2 s (post
   `single_shot_wifi::run` — was much longer when DHCP was unreliable).
   That's how long the pre-flash gets before the real flow `reset()`s
   the panel and aborts it (`src/bin/main.rs:346`). 2 s of an aborted
   ~20 s e-ink transition probably *does* show some visible movement
   on the panel, but it's worth checking whether it actually reads as
   feedback or just as flicker.
2. On the E1004 (1600×1200, 1.92 Mpx — five times the E1002's 384 kpx)
   even just the SPI push to load the panel buffer is meaningfully
   long, *and* every pre-flash kicks the panel into its high-current
   refresh state for those ~2 s before we abort it. On battery this
   adds up.

Worth measuring (active-mA-seconds spent on the pre-flash, on both
devices, plus a visual check of what the user actually sees) before
deciding. Then pick from:

- **Audible feedback instead of (or alongside) the visual flash.**
  Same hardware question as the config-mode-feedback TODO below —
  check the schematics for a piezo. A short tone is essentially free
  power-wise compared to a panel push. LEDs aren't an option: both
  devices have a status LED but it's on the backside, not user-facing.
- **Centred "waiting" icon via a partial-area update.** Instead of
  blanking the whole panel to white, draw a small icon (hourglass,
  spinner, the device logo, …) in the middle and leave the rest as
  whatever was last on the panel — both more informative as feedback
  *and* potentially much cheaper if we can drive a sub-region rather
  than the full 1.92 Mpx. Open hardware question: do the
  `Gdep073e01` / `T133A01` Spectra-6 controllers expose a partial-
  window refresh command? Multi-colour e-ink usually wants a
  full-panel drive sequence, so check the datasheets before
  committing — if partial-window isn't available, this collapses
  back to "send a centred-icon frame as a full refresh", which still
  reads better as feedback but doesn't save power.
- **Cancel earlier.** Today we abort by `reset()`-ing the panel after
  the config-mode race; if we could stop the in-progress
  `update_frame` SPI stream the moment the real flow wins, we'd save
  the rest of the push.

Pairs with the audible-feedback TODO below — deciding the buzzer
hardware story once would unblock both.

## Distinguishable buzzer feedback per event

`config_mode::run` already opens with an ~80 ms 2 kHz tone via the
GPIO45 piezo, so the "you can let go of the buttons now" cue is in.
Two follow-ups worth picking up later:

- **Short beep on every button press** — a quick tap-tone on
  Refresh / Previous / Next would confirm the press was registered,
  which is useful given the panel itself doesn't visibly react until
  the new frame paints (~2 s post-wake currently). Roughly the same
  envelope as the config-mode tone but shorter (~30–40 ms).
- **Longer / different tone on config-mode entry** — once button
  presses are also beeping, the existing single 80 ms tone would
  blend in with them. Differentiate the config-mode cue with a
  longer duration, a different pitch, or a two-tone pattern so the
  user can tell "registered a press" from "entered config mode".

LEDs aren't an option — the status LED is on the backside of the
device, not user-facing.

## Scan for nearby WiFi networks in the portal

The portal's SSID field is free-form text today. A dropdown populated
from a live scan is easier for the user, especially on phones without
a password manager that remembers their home network name. Needs
AP+STA concurrent mode on `esp-radio` — the controller supports
`ModeConfig::ApSta` but we've never exercised it (config mode is
AP-only), so expect some bring-up pain getting the STA side to scan
while the AP is serving the portal.

Probably gate behind a "Scan networks" button rather than scanning
on every form render; the cost is noticeable and we don't want to
block portal responsiveness. Keep a free-form text fallback for
hidden networks.

## Confirm `.local` / mDNS hostname resolution works

`embassy-net` is built with the `mdns` feature but we've never
verified the firmware actually resolves `.local` names on a real
network (e.g. `http://pegasus.local:3000/…` instead of the hostname
that happens to have DNS). Worth a smoke test — configure an mDNS-
advertised URL, set it via the portal, and check that
`embassy_net::dns::DnsSocket::query` returns an address. If it
doesn't, figure out whether we need an additional feature /
`embassy-net-mdns` crate or an explicit multicast subscription.

## `read_and_clear_rtc_gpio_wake_status` extraction

The PAC-poking helper still lives in `src/bin/main.rs`. With
`crate::uart::wait_for_tx_idle` already extracted as the first member
of a "low-level helper" cluster, this function is the obvious second
inhabitant — likely under `src/rtc_io.rs`. Worth doing once a second
caller materialises; right now it has only one user, so leaving it in
`main.rs` matches the "no shared module for a single value" rule.

## Validate config-mode settings before persisting them

When the user submits the portal form, `config_mode::run` writes
`wifi.ssid` / `wifi.pass` / `image.url` to NVS and reboots — the only
feedback they get that *anything* is wrong with the values is the
device booting straight into an error frame after the next cycle's
WiFi attempt fails. Pre-flight the new values *before* persisting so
the form can re-render with a useful message instead.

Suggested order (each step short-circuits on failure):

1. Try to associate with the new SSID/password using
   `single_shot_wifi::run`. If it errors out, surface "couldn't
   connect: …" in the form.
2. With WiFi up, do an HTTP GET against the supplied `image.url`,
   following the `try_fetch` classification (status code,
   `Content-Type`, `text/plain` = error message).
3. Run `try_decode_frame` on the body to confirm it's a panel-sized
   indexed PNG with a bit depth we accept. A successful decode is the
   strongest signal the URL is wired up correctly.

Only persist + reboot if all three pass; otherwise re-render the form
with the failing-step message inlined. The validation reuses
`crate::single_shot_wifi`, `crate::normal_mode::try_fetch`, and
`crate::normal_mode::try_decode_frame` — `try_fetch` /
`try_decode_frame` are currently `pub(crate)`-private to
`normal_mode`, so this entry implies promoting them to `pub(crate)`
exports.

## Drop `[patch.crates-io]` pins once upstream releases land

`Cargo.toml` currently patches two crates against upstream git commits
because the fixes we need are merged but not yet released:

- **`esp-nvs`** — pinned to `lhemala/esp-nvs` rev `e371b050…` (PR #24,
  bumps `esp-storage` from `0.8.1` → `0.9.0` to match the rest of the
  cascade). Released 0.4.0 still pins `esp-storage = "^0.8.1"`. Drop
  when a post-0.4.0 release ships.
- **`edge-nal`, `edge-nal-embassy`, `edge-http`, `edge-dhcp`,
  `edge-captive`** — all pinned to `ivmarkov/edge-net` master rev
  `1f084b41…` (PR #90, bumps to embassy-net 0.9 / heapless 0.9). The
  most recent crates.io releases (0.7.0 / 0.8.1 from 2026-01-05) still
  target embassy-net 0.8. Drop when a tagged release crossing that
  commit ships.

Check each on crates.io when doing the next `esp-hal` cascade upgrade
and remove whichever pins have been released out from under them.
