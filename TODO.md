# TODO

## Button-press latency on wake

The ESP-IDF second-stage bootloader takes ~300 ms from power-up to when our
app starts and can read GPIO state. Short button taps that are released
before that point fall through to the "no button still held" branch, and
the app defaults to assuming Refresh was pressed.

Workaround for now: hold the button until the screen starts flashing (the
white pre-flash is visual confirmation that the press was captured).

Future: research running a small code stub before the second-stage
bootloader — likely either via an ULP program or a deep-sleep wake stub
registered with `esp_deep_sleep_register_hook()` — that can latch the
wake-triggering GPIO into RTC memory so the main app can recover it even
if the button is already released by the time app code runs.

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

## Configurable WiFi credentials and URL

`WIFI_SSID`, `WIFI_PASSWORD`, and `WIFI_URL` are currently baked into the
binary via `env!`. They should be user-configurable at runtime, most
likely via a WiFi access-point captive portal (see the Previous+Next
30-second-hold placeholder in `main.rs`).
