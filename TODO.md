# TODO

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

## Button presses during refresh are ignored

During the ~20 s panel refresh the app is blocked on `wait_until_idle`, so
a button press during that window only takes effect on the *next* wake
cycle (after the current refresh finishes and the device deep-sleeps).

Consider handling this better — e.g. detect the press, reset the panel
(which aborts the in-progress refresh), and restart the main flow as if
this were a fresh wake. Concretely, this probably means running a
button-watching task concurrently with the refresh wait.

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

## `Spectra6Color::from_rgb` decision tree

`PanelColor::from_rgb` defaults to a closest-match search by squared
Euclidean distance over `all()` — six iterations per pixel. There used
to be a hand-tuned decision tree override on `Spectra6Color` that
short-circuited that with hard-coded thresholds:

```rust
fn from_rgb(value: Rgb888) -> Self {
    if value.r() < 105 {
        if value.b() < 109 {
            if value.g() < 62 {
                Spectra6Color::Black
            } else {
                Spectra6Color::Green
            }
        } else {
            Spectra6Color::Blue
        }
    } else if value.g() < 120 {
        Spectra6Color::Red
    } else if value.b() < 150 {
        Spectra6Color::Yellow
    } else {
        Spectra6Color::White
    }
}
```

It was never validated on hardware — the partition planes happen to
agree with `SPECTRA_6_PALETTE` for the six exact palette anchors but
the behaviour at off-anchor RGBs was never measured against the
closest-match version. We removed it for now in favour of the
verified default; the per-pixel cost of the default isn't measurable
in `try_decode_frame` because the lookup runs once per *PNG palette
entry* (max 256), not once per pixel.

To do, if we ever decide the per-palette closest-match cost matters:

1. Generate a corpus of test RGBs (uniform sampling and the typical
   server-emitted-then-dithered pixels we see in practice).
2. Compare the decision tree's output against the closest-match output
   pixel-for-pixel; find the points of disagreement.
3. Either tune the thresholds until they agree, or accept the
   disagreements as cosmetic and document why.
4. Restore the override in `src/spectra6.rs` with a test that pins the
   chosen thresholds.

## SY6974B charger reporting on E1002 / E1001

E1004 reads charger state from the SY6974B over I²C0 in `sensor_task`,
and the URL-builder appends `power=` only when that read is `Some`. The
cfg gate is currently `#[cfg(feature = "e1004")]`, with E1002 (and
prospectively E1001) opting out.

Per the Zephyr device trees for both 7" reTerminals
([E1001](https://github.com/zephyrproject-rtos/zephyr/blob/main/boards/seeed/reterminal_e1001/reterminal_e1001_procpu.dts),
[E1002](https://github.com/zephyrproject-rtos/zephyr/blob/main/boards/seeed/reterminal_e1002/reterminal_e1002_procpu.dts)),
both devices carry a SY6974B at `i2c1 0x6B` — the difference vs E1004
isn't "no charger", it's "different bus": E1004 puts the charger on
I²C0 (already up for the SHT40), while E1001 / E1002 put it on I²C1
(GPIO39 SDA / GPIO40 SCL, currently not initialised by the firmware).

To do:

1. Bring up I²C1 in `main()` alongside the existing I²C0 init; make
   `sensor_task` accept both buses.
2. Move the SY6974B read off I²C0 onto I²C1 for E1001 / E1002. E1004 stays
   on I²C0 — same driver, different bus instance.
3. Drop the `#[cfg(feature = "e1004")]` gate around the charger
   read; populate `POWER_STATUS` on all three devices.

DTS values to match if we ever write to `REG02` / `REG04`:
charge current 500 mA, charge voltage 4.208 V on E1001 / E1002 (the
E1004 DTS lists 1000 mA — a different battery pack).

## E1001 driver

The `Panel` / `PanelColor` traits in `src/panel.rs` already abstract
the panel-model differences, so adding the E1001 (grayscale GDEY075T7,
listed as "not implemented" in the README) is purely additive:

- A grayscale colour type (probably a 4- or 8-level enum) with
  `PanelColor` impl: `BLACK`, `WHITE`, `Default`, `all()`, `to_rgb`
  reading from a calibrated palette.
- A driver struct + `Panel<SPI>` impl analogous to `Gdep073e01`. No
  EN pin needed — `enable`/`disable` stay no-ops (the existing driver
  has none either).
- A third arm in `hardware.rs`'s `EpdPanel` cfg cascade plus a third
  cfg-gated `Output::new(...)` block in `main.rs` for the panel pins.

The image pipeline in main.rs is already generic over `EpdPanel`,
so no further consumer-side changes are needed — the new color type
flows through `PanelColor::from_rgb` for the PNG quantiser path.

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

## Clean up the UART-flush helper

`wait_for_uart_tx_idle` in `main.rs` mirrors
`esp_hal::uart::UartTx::flush` by polling the UART0 PAC directly —
it works, but the raw `esp_hal::peripherals::UART0::regs()` reads
feel out of place in otherwise peripherals-owned-via-`esp_hal::init`
application code. Ideally we'd grab `peripherals.UART0` in `main()`,
wrap it in a real `UartTx`, thread it through `HardwareCtx`, and
call its `flush()` before `sleep_deep`.

The only snag is that `esp-println` writes to UART0 via ROM
functions rather than taking the peripheral through the PAC, so
both "owners" share the hardware — in practice they cooperate
because the ROM functions just poke registers, but the
peripherals-ownership story would get a bit hand-wavy. Worth
looking at what `esp_hal::uart::UartTx::new` actually does to see
whether splitting the registers / using the existing `Uart` type
alongside esp-println is explicitly supported.

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
