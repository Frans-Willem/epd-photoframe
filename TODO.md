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

## WiFi connection failure is invisible

If the configured SSID / password is wrong, `wifi_task` loops on
`connect_async()` forever (retrying every 5 s) and `main()` blocks on
`net_stack.wait_link_up().await`, so the device is stuck with no output on
the panel — same visible state as "still booting" from the user's
perspective. Should be caught and surfaced: wrap the wait in a timeout
(say 30 s from first attempt), and on timeout render an error frame via
`error_image::render` explaining that WiFi connection failed, then go to
deep sleep (so we retry on the next wake rather than burning battery in
the retry loop). Once runtime WiFi configuration lands, this error frame
should also hint at entering config mode (hold Previous+Next for 10 s).

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
10-second-hold placeholder in `main.rs`).
