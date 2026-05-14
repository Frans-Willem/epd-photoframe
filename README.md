# epd-photoframe

Full-fledged photo frame firmware for Seeed reTerminal E1001, E1002, and
E1004 e-paper devices.

epd-photoframe turns the reTerminal E100x boards into battery-powered
digital photo frames. The device wakes up, connects to WiFi, fetches one
already-prepared PNG image, refreshes the e-paper panel, and goes back to
deep sleep until the next refresh.

The firmware is designed to be used with
[epd-photoframe-server](https://github.com/Frans-Willem/epd-photoframe-server),
which turns a public Google Photos album into correctly sized, dithered
images for these panels.

## How it fits together

Three pieces make up the usual setup:

- **epd-photoframe** *(this repo)* runs on the frame. It handles WiFi,
  buttons, battery and sensor reporting, image download, panel refresh,
  and deep sleep.
- **[epd-photoframe-server](https://github.com/Frans-Willem/epd-photoframe-server)**
  runs on a server, NAS, Raspberry Pi, or other always-on machine. It
  picks photos from a Google Photos album, crops or pads them, draws
  optional overlays, dithers them to the target panel palette, and
  returns a PNG.
- **[epd-dither](https://github.com/Frans-Willem/epd-dither)** does the
  palette conversion. The server uses it directly; you do not need to
  install it separately for the frame firmware.

The frame intentionally does very little image processing itself. This
keeps the firmware small, reduces wake time, and avoids running into RAM
limits on the large 1200x1600 E1004 panel.

## Supported devices

| Binary | Device | Panel | Resolution |
|--------|--------|-------|------------|
| `e1001` | reTerminal E1001 | GDEY075T7 grayscale e-paper | 800x480 |
| `e1002` | reTerminal E1002 | GDEP073E01 Spectra 6 e-paper | 800x480 |
| `e1004` | reTerminal E1004 | T133A01 Spectra 6 e-paper | 1200x1600 |

All three devices are supported. The E1001 uses 4-level grayscale; the
E1002 and E1004 use six-colour Spectra 6 panels.

## What it does

- Fetches a paletted PNG over HTTP and displays it on the e-paper panel.
- Uses the server's `Refresh` header to decide when to wake up again.
- Reports battery voltage, battery percentage, temperature, humidity,
  and available charger state as query parameters on each image request.
- Supports hardware buttons for refresh, previous photo, and next photo.
- Provides a setup WiFi network and captive portal for configuring WiFi
  and the image URL.
- Enters deep sleep between refreshes for long battery life.

## Before flashing

You need:

- A supported Seeed reTerminal E1001, E1002, or E1004.
- A USB-C cable for flashing and serial logs.
- A Rust ESP toolchain that can build for `xtensa-esp32s3-none-elf`.
- `espflash` available in your shell.
- An image endpoint that returns a panel-sized paletted PNG. For normal
  use, run
  [epd-photoframe-server](https://github.com/Frans-Willem/epd-photoframe-server)
  and point the frame at `/screen/<name>`.

The firmware expects plain `http://` URLs. The setup portal rejects
`https://` image URLs.

## Flashing

Pick the binary for your device:

```bash
cargo run --release --bin e1001
cargo run --release --bin e1002
cargo run --release --bin e1004
```

The repository's Cargo configuration uses `espflash flash --monitor` as
the runner, so `cargo run` flashes the device and opens the serial
monitor.

If your serial port is not auto-detected, pass it through to `espflash`:

```bash
cargo run --release --bin e1004 -- --port /dev/ttyUSB0
```

## First setup

On a freshly flashed device, or whenever required configuration is
missing, the frame enters setup mode automatically.

1. The frame shows a setup screen with a QR code.
2. Scan the QR code, or join the WiFi network named
   `epd-photoframe-setup-XXXX`.
3. Open `http://192.168.4.1/` if the captive portal does not open
   automatically.
4. Enter your WiFi SSID, WiFi password, and image URL.
5. Save the form. The device restarts and begins fetching images.

To re-enter setup later, reboot the device and hold **Previous** and
**Next** for 10 seconds during boot.

## Image endpoint

The configured URL should return a PNG that already matches the target
device's resolution and palette. The firmware does not resize, crop, or
dither normal photos on-device.

On each request, the firmware may add these query parameters:

- `battery_mv`
- `battery_pct`
- `temp_c`
- `humidity_pct`
- `power`
- `action=refresh`, `action=previous`, or `action=next`

The response can include a standard HTTP `Refresh` header. The firmware
uses that interval for the next timed wake-up, and also supports
`Refresh: <seconds>; url=<url>` to move to a different URL.

## Buttons

- **Refresh** wakes the frame immediately and requests
  `action=refresh`.
- **Previous** requests `action=previous`.
- **Next** requests `action=next`.
- Holding **Previous** + **Next** for 10 seconds during boot enters
  setup mode.

Button actions are most useful with epd-photoframe-server, which maps
them to refresh, previous-photo, and next-photo behaviour.

## Power and battery life

The firmware is intended for slow refresh schedules: a few times per day
rather than every few minutes. With the measured E1001, E1002, and E1004
hardware, expected battery life is on the order of months when waking
once or twice per day.

See [POWER.md](./POWER.md) for measured sleep current, wake-cycle cost,
and battery-life estimates.

## Hardware notes

Panel references, schematics, and lower-level hardware notes live in
[HARDWARE.md](./HARDWARE.md). They are useful when changing panel
drivers or investigating board behaviour, but are not needed for normal
use.

## A note on LLM use

I made heavy use of Claude while building this. I was closely involved
throughout - every change Claude proposed was reviewed before it landed,
and I personally stand by the quality of the code in this repo. What an
LLM gave me was time: enough of it to take this project from "works on
my desk" to something polished enough for other people to use, which I
wouldn't otherwise have done as a side project.

That said, plenty of suggestions Claude made along the way were
nonsense, and I would not trust an LLM to write code unsupervised after
this experience. Use accordingly.
