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

## `Refresh` header URL override needs RTC-retained memory

The `Refresh: <secs>[; url=<new URL>]` header is parsed and the
interval is used for the next deep-sleep, but the `url` parameter is
currently just logged and dropped. The user's intent: let the server
hand out a one-shot override URL (e.g. "next refresh, serve this
different page") that doesn't overwrite the configured base in NVS.

Needs RTC-retained memory — content that survives deep sleep but is
lost on power-off, which is the right TTL for a single-shot override.
Wire up an `#[link_section = ".rtc_noinit"]` static (or an equivalent
esp-hal primitive) and, on wake, check it *before* reading `image.url`
from NVS. Same scaffolding will be useful for the DHCP-lease cache
(see separate TODO).

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

## Persist DHCP lease in RTC memory

Once the `Refresh`-header work has RTC-retained memory wired up for
the one-shot URL override, it'd be cheap to also stash the current
DHCP lease (IP, mask, gateway, DNS, lease TTL) there. On the next
wake we'd skip the DHCP request entirely (`embassy_net::Config::
ipv4_static` with the retained values) as long as the lease hasn't
expired and the network is presumably the same. Saves a round-trip
(~100-300 ms on a decent AP) per wake — noticeable over a day of
timer-driven refreshes.

Fall back to DHCP on: lease expired, retained values unreadable, or
any failure on the first probe (e.g. gateway no longer responding).
Sketch the fall-back as "try the static config, ping gateway, on
failure tear down + redo with DHCP" — straightforward enough once
the RTC-memory scaffolding exists.

## Button presses during refresh are ignored

During the ~20 s panel refresh the app is blocked on `wait_until_idle`, so
a button press during that window only takes effect on the *next* wake
cycle (after the current refresh finishes and the device deep-sleeps).

Consider handling this better — e.g. detect the press, reset the panel
(which aborts the in-progress refresh), and restart the main flow as if
this were a fresh wake. Concretely, this probably means running a
button-watching task concurrently with the refresh wait.

## Power saving

The WiFi radio stays on through the entire refresh even though we don't
need it after the image has been fetched. Turn it off as soon as
`try_build_frame` returns. More generally: audit what's still powered
between "image in memory" and "deep sleep" and shut down anything we
don't need.

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

## Flush `println!` output before deep sleep / software reset

`esp_println::println!` writes to UART, but we never check that the
final log lines are fully clocked out before `rtc.sleep_deep` or
`esp_hal::system::software_reset()`. In practice the last message
before reboot ("Going to deep sleep :)", "Rebooting", etc.) often
appears truncated in the serial monitor, which is exactly the window
where a diagnostic is most useful.

Research whether the esp-hal UART has a drain / wait-for-idle API we
can await before pulling the trigger, or whether a short
`embassy_time::Timer::after` (~10 ms at the current baud) is a
reliable enough proxy. Apply the same to the normal-flow
`sleep_deep` and the config-mode `software_reset` paths.

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

## Dependency upgrade cascade blocked on `esp-hal 1.1.0-rc → 1.1.0`

The `esp-hal` / `esp-rtos` / `esp-radio` / `esp-alloc` / embassy stack is
tightly coupled. After the Stage 3b audit:

| crate                   | our version | latest stable | blocker                             |
|-------------------------|-------------|---------------|-------------------------------------|
| esp-hal                 | 1.0.0       | 1.0.0 (1.1.0-rc.0 published) | — |
| esp-rtos                | 0.2.0       | 0.3.0         | 0.3 requires esp-hal 1.1.0-rc.0     |
| esp-radio               | 0.17.0      | 0.18.0        | 0.18 requires esp-hal 1.1.0-rc.0    |
| esp-bootloader-esp-idf  | 0.4.0       | 0.5.0         | 0.5 requires esp-hal 1.1.0-rc.0     |
| esp-storage             | 0.8.1       | 0.9.0         | 0.9 requires esp-hal 1.1.0-rc.0     |
| esp-alloc               | 0.9.0       | 0.10.0        | esp-rtos 0.2 pins ^0.9              |
| embassy-executor        | 0.9.1       | 0.10.0        | esp-rtos 0.2 pins ^0.9              |
| embassy-time            | 0.5.0       | 0.5.1         | 0.5.1 requires embassy-executor 0.10 |
| embassy-net             | 0.8.0       | 0.9.1         | 0.9 requires embassy-time 0.5.1     |
| smoltcp                 | 0.12        | 0.13          | esp-radio 0.17 pins ^0.12           |
| heapless (direct dep)   | 0.8         | 0.9.2         | embassy-net 0.8 uses ^0.8           |

When `esp-hal 1.1.0` ships stable, re-run this audit — a single cascade
should be able to lift `esp-rtos` / `esp-radio` / `esp-alloc` /
`esp-storage` / `esp-bootloader-esp-idf` to their newest stables, and with
them the embassy + smoltcp + heapless + leasehund stack. Pre-release
versions are deliberately not used (see memory: no rc/beta dep versions).
