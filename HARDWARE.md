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
