# TODO

## Exit config mode without saving

Once the device is in config mode the only way out without committing
new credentials is a full power-cycle, which is awkward if the user
walked into config mode by accident (or just wants to leave the
existing NVS values in place). Wire up the Refresh button to perform
a software reset while in config mode, mirroring the save-and-reset
path but skipping the NVS write. Watch for a Refresh press via an
async task racing against the portal's `SAVE_SIGNAL`; whichever fires
first wins.

The panel instructions + the portal HTML should both mention this:
"Press Refresh to leave config mode without changes." Check that the
instruction layout still fits on the portrait E1004 when extended.

## Honour a `Refresh` header on the image response

The image server may return a `Refresh: <seconds>[; url=<new URL>]`
header (same semantics as the HTML meta-refresh equivalent). Three
follow-ups for the firmware:

1. **Use that interval for the next deep-sleep** instead of the
   hard-coded 10 minutes, so the server can throttle / accelerate
   updates server-side without a firmware change.
2. **If a URL is supplied**, stash it in RTC-retained memory so the
   next wake fetches *that* URL instead of `image.url` from NVS. The
   user's intent: let the server hand out a one-shot override URL
   (e.g. "next refresh, serve this different page") that doesn't
   overwrite the configured base. Anything in RTC memory is lost on
   power-off, which is the right TTL for a single-shot override.
3. **Reconsider the default sleep duration** we fall back to when the
   server doesn't send a `Refresh` header. 10 minutes is aggressive
   for a successful image (e-paper is fine with hours-scale updates);
   keep 10 minutes for the error-frame path (user will probably want
   to retry soon), but stretch the happy path to something like 2-6 h.

Also fix the URL-action joiner: `try_build_frame` currently appends
`?action=...` unconditionally, which breaks URLs that already carry a
query string. Check whether the URL already contains a `?` and use
`&action=...` when it does.

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
- **`arrayvec` is redundant.** It's used in exactly two places — the
  GDEP073E01 and T133A01 drivers each hold a 128-byte
  `ArrayVec<u8, 128>` scratch buffer for stacking command + data bytes
  before firing them over SPI. `heapless::Vec<u8, 128>` would do the
  same job with the crate we're already pulling in. Swap the two uses
  and drop `arrayvec` from the direct deps list.

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
