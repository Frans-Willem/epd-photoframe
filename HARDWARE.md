# Hardware Notes

This file collects panel references, schematics, and implementation notes
that are useful when working on the firmware. Normal setup instructions
live in [README.md](./README.md).

## Device Schematics

The E1001, E1002, and E1004 are all ESP32-S3 based devices with similar
peripherals and different panel pinouts.

- E1001:
  https://files.seeedstudio.com/wiki/reterminal_e10xx/res/202004307_reTerminal_E1001_V1.0_SCH_250805.pdf
- E1002:
  https://files.seeedstudio.com/wiki/reterminal_e10xx/res/202004321_reTerminal_E1002_V1.0_SCH_250805.pdf
- E1004:
  https://files.seeedstudio.com/wiki/reterminal_e10xx/res/202004523_reTerminal%20E1004_V1.0_SCH_260105.pdf

## Panels

| Device | Panel | Notes |
|--------|-------|-------|
| E1001 | GooDisplay GDEY075T7 | 800x480, 4-level grayscale |
| E1002 | GooDisplay GDEP073E01 | 800x480, Spectra 6 |
| E1004 | T133A01 | 1200x1600, Spectra 6, dual-controller |

Panel references:

- E1001 GDEY075T7: https://www.good-display.com/product/396.html
- E1002 GDEP073E01: https://www.good-display.com/blank7.html?productId=533
- E1004 T133A01 datasheet from Seeed:
  https://files.seeedstudio.com/wiki/Other_Display/1330-E6-epaper/13.3_E6_eInk_Display_module_Datasheet.pdf

The E1004 panel is a 13.3 inch 1200x1600 Spectra 6 panel driven through
two controllers. See `src/panel/t133a01.rs` for the current command
sequence.

## E1004 and GDEP133C02

Good Display's GDEP133C02 ESP-IDF example is a useful comparison point,
but it does not prove that the E1004's T133A01 panel is the same module:

https://www.good-display.com/companyfile/1755.html

The example matches the broad driver model used by the E1004:

- 1200x1600 Spectra 6 image data, packed as two pixels per byte.
- Two chip selects for two panel controllers.
- Image data split into left and right controller halves.
- The same main command set: `PSR` (`0x00`), `PWR` (`0x01`), `POF`
  (`0x02`), `PON` (`0x04`), `DTM` (`0x10`), `DRF` (`0x12`), `CDI`
  (`0x50`), `TCON` (`0x60`), `TRES` (`0x61`), `AN_TM` (`0x74`),
  `AGID` (`0x86`), `B0`, `B1`, `B6`, `B7`, `CCSET` (`0xE0`), `PWS`
  (`0xE3`), and `F0`.
- Very similar init flow: analog timing, `F0`, panel setting, border /
  TCON / resolution setup, power settings, booster setup, then data
  transfer and refresh.

Important differences remain:

- Good Display uses `CDI = 0xF7`; the current E1004 driver uses `0x37`.
- Good Display's `AN_TM` bytes differ from the current driver.
- Good Display's booster soft-start values are `0xE8, 0x28`; the
  current driver uses `0xE0, 0x20`.

Treat GDEP133C02 as a related command-family reference unless panel
markings, vendor confirmation, or a full pinout/mechanical match proves
that it is the same module as the E1004's T133A01.

## Other References

- Rust library for the GDEP073E01 with useful command names:
  https://github.com/xandronak/gdep073e01/blob/main/src/lib.rs
- Another ACEP / seven-colour display with a similar command set,
  useful for the `PanelSetting` scan-order bit:
  https://github.com/robertmoro/7ColorEPaperPhotoFrame/blob/main/7ColorEPaperPhotoFrame/epd5in65f.cpp
- Write-up on the technologies behind ACEP:
  https://hackaday.io/project/179058-understanding-acep-tecnology

## Development Notes

All three panel driver modules are compiled regardless of which binary is
selected. This intentionally makes driver compile errors visible even
when building for only one device:

```bash
cargo build --bin e1001
cargo build --bin e1002
cargo build --bin e1004
```
