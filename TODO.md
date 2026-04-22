# TODO

## PNG bit-depth handling

`try_build_frame` currently assumes the PNG is **indexed 8 bpp** (256
palette entries, one byte per pixel). The server is being extended to
emit 1/2/4/8 bpp indexed PNGs, so the decoder-side pixel reading needs
to branch on `image.bit_depth()` and unpack the row bytes accordingly.
The palette-lookup logic stays the same — only the per-pixel byte →
palette-index extraction changes. Worth also adding a guard so an
unexpected bit depth or a non-indexed colour type surfaces as an
error frame rather than corrupt pixels.

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

## Unify networking on the `edge-net` stack

We currently mix unrelated networking crates across both modes:

- Config mode: `leasehund` (DHCP), `edge-captive` (DNS), and — at the
  time of writing — about to add an HTTP portal.
- Normal mode: `reqwless` for the image-fetch HTTP client.

The `edge-net` family ships `edge-dhcp` (DHCP server), `edge-captive`
(DNS — already in use), `edge-http` (server *and* client), and
`edge-mdns`, all built on the same `edge-nal` abstraction we already
pull in via `edge-nal-embassy`. Swapping each of the non-edge-net
crates for its edge-net counterpart would collapse the dep graph onto a
single networking trait and buffer-pool style.

Concrete migrations to evaluate together:

1. **`leasehund` → `edge-dhcp`** (config-mode DHCP server).
2. **`picoserve` → `edge-http` server** for the config portal — see the
   Stage 3c choice made in PLAN.md; if the ergonomics pan out, this is
   already aligned with the rest of the plan.
3. **`reqwless` → `edge-http` client** for the normal-flow image fetch.
   This is the bigger ergonomics hit (we'd lose reqwless's body reader
   + content-type parsing), so it depends on how painful the config-
   portal's `edge-http` server code ends up being.

Worth prototyping all three once Stage 3c is stable — if the per-crate
code size stays reasonable, the dep simplification + consistent error
types are probably worth it.

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
| leasehund               | 0.3.0       | 0.4.0         | 0.4 requires embassy-net 0.9        |
| heapless (direct dep)   | 0.8         | 0.9.2         | embassy-net 0.8 uses ^0.8           |

When `esp-hal 1.1.0` ships stable, re-run this audit — a single cascade
should be able to lift `esp-rtos` / `esp-radio` / `esp-alloc` /
`esp-storage` / `esp-bootloader-esp-idf` to their newest stables, and with
them the embassy + smoltcp + heapless + leasehund stack. Pre-release
versions are deliberately not used (see memory: no rc/beta dep versions).
