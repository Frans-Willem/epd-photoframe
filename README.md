reTerminal E100x playground
===========================
Purpose
-------
Playing around with Rust, ESP32, Embassy, and eInk.

Goal
----
Stand-alone firmware that will:
- Wake up every 10 minutes (configurable), download a pre-dithered palette PNG image, and display it.
- On each wake-up all sensors (battery, temperature, humidity) should be read and reported (MQTT, HTTP POST, or as headers of the PNG GET).
- Low power consumption (deep sleep) between wake-ups.
- Buttons should allow forcing a wake-up and refresh (green) or changing between pages (e.g. different URLs).
- WiFi settings and URLs are configurable through an Access Point captive portal.
- Captive portal is entered automatically on a freshly-flashed device (whenever any required NVS key is missing) and can also be re-entered at any boot by holding Previous+Next for 10 s.

Originally the full-colour PNG was going to be dithered on the device, but the
larger E1004 panel (1200×1600) didn't leave enough memory for that approach,
so image processing has moved off-device: the firmware now expects a
palette-based PNG that is already quantised to the target device's palette.

Stretch goals:
- TRMNL compatibility, allowing switching between TRMNL and other URLs via a long-press of the left/right buttons.
- Load and display images/folders from SD card.
- Unless triggered by the Refresh button, only refresh the portion of the screen that actually changed (use ESP "RTC" RAM?).

Supported devices
-----------------
Build with `cargo build --features <device>` (exactly one device feature
must be enabled — there is no default):

| Feature | Device      | Panel     | Resolution  | State       |
|---------|-------------|-----------|-------------|-------------|
| `e1001` | reTerminal E1001 (7")  | GDEY075T7  | 800×480 (4-level grayscale) | working    |
| `e1002` | reTerminal E1002 (7")  | GDEP073E01 | 800×480  | working     |
| `e1004` | reTerminal E1004 (13") | T133A01   | 1200×1600 | working     |

All three panel driver modules are always compiled regardless of the
selected feature, so changes surface compile errors in every driver.

Progress
--------
Firmware runs on the E1002 and E1004. WiFi credentials and image URL are
provisioned at runtime through the captive-portal web UI and persisted to
ESP-IDF NVS. The device refreshes every 10 minutes and on button press,
fetching a palette-based PNG from the configured URL.

References
----------
Schematics (ESP32-S3 based, similar peripherals, differ in panel pinout):
- E1001: https://files.seeedstudio.com/wiki/reterminal_e10xx/res/202004307_reTerminal_E1001_V1.0_SCH_250805.pdf
- E1002: https://files.seeedstudio.com/wiki/reterminal_e10xx/res/202004321_reTerminal_E1002_V1.0_SCH_250805.pdf
- E1004: https://files.seeedstudio.com/wiki/reterminal_e10xx/res/202004523_reTerminal%20E1004_V1.0_SCH_260105.pdf

Panels:
- E1001: GooDisplay GDEY075T7 — https://www.good-display.com/product/396.html
- E1002: GooDisplay GDEP073E01 — https://www.good-display.com/blank7.html?productId=533
- E1004: T133A01 (1200×1600, Spectra 6, dual-controller)

Other references:
- Rust library for the GDEP073E01 with nice command names: https://github.com/xandronak/gdep073e01/blob/main/src/lib.rs
- Another ACEP (7-colour) display with a similar command set, useful for the PanelSetting scan-order bit:
  https://github.com/robertmoro/7ColorEPaperPhotoFrame/blob/main/7ColorEPaperPhotoFrame/epd5in65f.cpp
- Write-up on the technologies behind ACEP:
  https://hackaday.io/project/179058-understanding-acep-tecnology
