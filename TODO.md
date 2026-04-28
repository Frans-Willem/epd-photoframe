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

## Report battery / sensor readings to the server

The README's Goal section has always called for per-wake sensor
reporting (battery, temperature, humidity) but the firmware currently
fetches the image with no side-band. Send the readings on every
image GET — either as query-string params, HTTP headers, or a POST
body — so the server can decide what to render based on current
state (e.g. low-battery overlay, trending graphs).

Likely shape:

- Read ADC for battery voltage (plus whatever sensors are on the
  I²C bus — check schematics).
- Serialise into short header values:
  `X-Battery-mV: 3920`, `X-Temp-C: 22.4`, etc.
- Drop them into the `initiate_request` headers in `try_build_frame`.

Keep the query-string-param backup in mind for servers / clients that
strip custom headers.

## Harmonise padding between the QR and instructions area

On the config-mode screen the QR code sits inside a 32 px margin (see
`qr_image::MARGIN_PX`) while the instructions start at
`Point::new(16, 16)` inside their area with no right / bottom margin
at all. Visually the text strip reads as noticeably "tighter" than
the QR strip.

Pull both values out of their respective modules into a shared layout
constant (say 24 px) and apply it consistently: the QR's centring
margin and the text's top-left offset *and* right / bottom insets
when wrapping. Pairs naturally with the word-aware-wrap TODO above,
since the wrapper needs the effective text bounding box anyway.

## Error frame should advertise the next retry time

`error_image::render` shows the failure reason but doesn't tell the
user when the device will try again. On the error path `main_normal`
schedules a retry via `DEFAULT_ERROR_SLEEP` (10 min today) or whatever
the server's `Refresh:` hint said — but that interval is invisible to
whoever's looking at the panel. Add a line like "Will retry in
10 minutes" / "Will retry at 14:05" to the error frame so the user
knows whether to wait or to hit Refresh themselves.

Implementation-wise: thread the planned wake `Instant` (or the
`Duration` from now) into `error_image::render` and render it as an
extra line below the error text. Pairs naturally with the word-aware
text wrapping TODO below, since the extra line wants the same wrapper.

## Word-aware text wrapping for panel instructions + error frames

Both `config_image::render` (the QR + instructions page) and
`error_image::render` use a naive wrapping model:

- `config_image`: the instruction text is *hand-wrapped* with explicit
  `\n` line breaks sized for the narrow landscape E1002 layout. On the
  E1004 the text strip below the QR is much wider (the QR is at the
  top of a portrait panel), so those short lines look cramped and
  ragged.
- `error_image`: `hard_wrap` breaks at an exact character count,
  happily splitting mid-word.

Replace both with a word-aware wrapper that takes the text-area
bounding box (in pixels) and the font's character dimensions, then
greedily packs words, falling back to mid-word breaks only for words
that don't fit a line on their own. The wrapped string then feeds
`embedded_graphics::Text::with_baseline` the same way today. Both
call sites can share the wrapper since they both use `FONT_10X20`.

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
(octal rail), the panel SPI bus after `power_off`, any sensor /
ADC / I²C peripherals that might be left initialised. Battery /
sensor reporting (the TODO above) is worth doing before the audit
so we're not turning off things we're about to need.

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

## Panel-trait abstraction for multi-palette support

The `Gdep073e01` and `T133A01` drivers currently expose duplicate
method surfaces that main.rs / config_mode.rs call through cfg-gated
type aliases (`EpdPanel`, `use … as panel`). That works for the two
Spectra-6 panels we ship today, but the E1001 (grayscale GDEY075T7,
listed as "not implemented" in the README) has a different palette
entirely — bolting it on would need yet another cfg arm in every
caller.

Consolidate by introducing a `Panel` trait that both drivers implement:

- Method surface: `reset`, `init`, `power_on`, `update_frame(impl
  IntoIterator<Item = Self::PanelColor>)`, `display_frame_no_wait`,
  `wait_until_idle`, `power_off`, plus the `panel_size()` and
  `output_index_to_image_xy()` helpers lifted into associated
  functions.
- Associated type `PanelColor` that exposes:
  - `const BLACK: Self` and `const WHITE: Self` (used by the error /
    white-preflash paths),
  - `fn all() -> impl Iterator<Item = Self>` (used by the PNG palette
    quantiser to build a lookup table over every value the panel can
    display),
  - `fn to_rgb(self) -> [u8; 3]` (same, for closest-colour matching
    against PNG palette entries).

That drops `EpdPanel` as a cfg-gated alias and lets main.rs / the
image pipeline be generic over the panel. The E1001 driver then only
needs to provide its own PanelColor (probably an 8-level grayscale
enum) and slot in.

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

## Audible / visible "config mode entered" feedback

Holding Previous+Next for 10 seconds without any indication that the
device noticed is unnerving — the user can't tell whether to keep
holding or if they've already succeeded (the e-ink doesn't re-paint
until a second or two later). Research options to give immediate
feedback the moment `entering_config_mode` resolves true:

- **Beep.** Check the E1002 / E1004 schematics for a piezo /
  speaker — ESP32-S3 has both a LEDC PWM peripheral and DACs that
  could drive one. If there's hardware, a short tone (~50 ms) at the
  race-end is the clearest signal.
- **LED pattern change.** The status LED on GPIO6 already blinks via
  `blink_task`. As a fallback if there's no audio path, switch to a
  faster blink (or solid-on) when entering config mode; revert on
  exit.

Pick whichever the hardware actually supports with minimal extra
wiring.

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

## Re-audit direct dependencies

Some direct `[dependencies]` entries were added for crates that have
since been ripped out (e.g. `heapless` came in with `leasehund`; that's
gone, but we still use one `heapless::Vec::new()` call in
`config_mode.rs` to build `StaticConfigV4.dns_servers`). Worth a sweep
to confirm each direct dep has a real call site that isn't served by a
re-export we already have in the tree.

Specifics to address:

- **`heapless` is mandatory** (embassy-net's `StaticConfigV4.dns_servers`
  is typed as `heapless::Vec<Ipv4Address, 3>`, and embassy-net doesn't
  re-export the type).  Dropping it would mean stopping the use of
  `Config::ipv4_static` entirely, which isn't worth it.

## Drop `[patch.crates-io]` pins once upstream releases land

`Cargo.toml` currently patches two crates against upstream git commits
because the fixes we need are merged but not yet released:

- **`esp-nvs`** — pinned to `lhemala/esp-nvs` rev
  `e371b050…` (PR #24, loosens the `esp-hal = "1.0.0"` requirement so
  Cargo's pre-release rule accepts 1.1.0-rc.0). Drop when a post-0.4.0
  release ships.
- **`edge-nal`, `edge-nal-embassy`, `edge-http`, `edge-dhcp`,
  `edge-captive`** — all pinned to `ivmarkov/edge-net` master rev
  `1f084b41…` (PR #90, bumps to embassy-net 0.9 / heapless 0.9). Drop
  when a tagged release crossing that commit ships.

Check each on crates.io when doing the next `esp-hal` cascade upgrade
and remove whichever pins have been released out from under them.

## Move off `esp-hal 1.1.0-rc.0` once 1.1.0 ships stable

We pulled in the rc deliberately for its calibrated RTC-slow sleep
fix (see Cargo.toml comment) — all the cascaded esp-* / embassy / edge
crates came with it. When `esp-hal 1.1.0` drops the `-rc` suffix,
bump our pin to that (plain `"1.1.0"` or the then-current minor) and
re-check each sibling for a matching stable release. The
`[patch.crates-io]` entries for `esp-nvs` and the `edge-*` crates
should be droppable at the same time (see the entry above).
