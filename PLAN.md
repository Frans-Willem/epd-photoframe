# Runtime configuration via captive portal — plan

## Goal

Replace the compile-time `env!("WIFI_SSID")` / `env!("WIFI_PASSWORD")` /
`env!("WIFI_URL")` with runtime-editable configuration, provisioned through a
captive portal served by the device itself when it runs in configuration mode.

## End-user experience

On a freshly-flashed device, or when the user wants to change config, the
device enters **configuration mode**:

- It brings up an open WiFi AP with an obvious name (e.g.
  `reTerminal-setup-XXYY`, last two bytes of the MAC).
- It shows a full-screen QR code on the eInk panel so the user can join the
  AP with one tap from their phone.
- Joining the AP automatically opens the captive portal (the phone's OS
  thinks the network needs "sign-in").
- The portal shows a form: WiFi SSID, password, image URL. Submit → save →
  reboot.
- After the user submits, the device software-resets; on the subsequent
  boot the wake reason is `Undefined`, so `determine_wake_action` returns
  `FreshBoot`, and the normal `FreshBoot` path (white pre-flash → fetch
  → real refresh) runs — that already gives the "panel refreshed to
  white" visual confirmation that config mode has exited, so no
  special-case painting is needed in the config flow.
- While in config mode: the status LED keeps blinking. The panel image (QR
  code) is persistent — eInk stays visible even though the device is busy
  serving the portal — which doubles as visual confirmation that the device
  is up and listening.

## Triggers for entering configuration mode

Either of the following, evaluated at the top of `main()`:

1. **No config stored** (first boot after a flash, or user reset).
2. **Previous + Next physically held at boot for 30 s** — completely
   independent of the RTC latch / `WakeAction` machinery. Checked against
   the live GPIO levels so it works uniformly for any boot reason — cold
   power-on with both buttons held while flipping the switch, deep-sleep
   wake with them pressed, or software reset.

   Implementation is async, not a busy loop: once the executor is running,
   if the initial `previous_held && next_held` was true, race three
   futures and act on whichever resolves first:

   ```
   match select3(
       previous_input.wait_for_high(),   // Previous released
       next_input.wait_for_high(),       // Next released
       Timer::after(Duration::from_secs(30)),
   ).await {
       First(_) | Second(_) => (),       // fall through to normal flow
       Third(_)             => enter_config_mode(),
   }
   ```

   While waiting, the existing `blink_task` keeps running so the LED
   confirms the device is alive and gives the user something to hold until.

## Storage

### Crate stack

- **`esp-storage`** — `embedded-storage::NorFlash` implementation for the
  internal flash.
- **`esp-nvs`** (v0.4.0, MIT/Apache-2.0) — ESP-IDF NVS **binary-compatible**
  key-value store on top of `esp-storage`. Pure Rust, `no_std`. Same wire
  format ESP-IDF uses, so the existing 24 KB `nvs` partition at `0x9000` in
  our partition table is usable as-is without a partition-table change, and
  bulk provisioning via `esptool.py` / `nvs_partition_gen.py` stays an
  option.

Values live under **one NVS namespace of our own** (e.g. `"config"`), one
entry per field, rather than a single serialised blob — simpler to read,
write, and evolve (adding a field is a new key, not a schema version).
`esp_wifi`'s internal NVS namespace (`nvs.net80211`) is deliberately not
reused: it's an undocumented IDF-internal layout that doesn't fit our URL
or static-IP fields and would tie us to IDF behaviour we don't otherwise
need.

### Key schema

Namespace `"config"`:

| Key         | Type | Notes                                                     |
|-------------|------|-----------------------------------------------------------|
| `wifi.ssid` | str  | Required for STA mode. UTF-8.                             |
| `wifi.pass` | str  | Empty / missing means open network.                       |
| `wifi.auth` | str  | Optional; `"open"` or `"wpa2"`. Auto-detect from `.pass` if absent. |
| `image.url` | str  | The HTTP(S) URL we fetch on every wake.                   |
| `net.mode`  | str  | `"dhcp"` (default) or `"static"`.                         |
| `net.addr`  | u32  | IPv4 address, big-endian. Only if `net.mode == "static"`. |
| `net.mask`  | u8   | CIDR prefix length (e.g. 24). Only if static.             |
| `net.gw`    | u32  | IPv4 gateway. Only if static.                             |
| `net.dns1`  | u32  | Optional DNS server.                                      |
| `net.dns2`  | u32  | Optional second DNS server.                               |

### Static vs DHCP

`embassy_net` supports both via `Config::dhcpv4(..)` and
`Config::ipv4_static(StaticConfigV4 { address, gateway, dns_servers })`;
the switch is just which variant we pass in.

**Stage 3 form ships DHCP-only** (single config form, no IP fields). The
`net.*` keys are part of the schema from day one so static support can be
added later by exposing the extra form fields, no schema migration needed.
Missing `net.mode` is read as `"dhcp"`.

## Configuration mode internals

### WiFi AP

`esp-radio::wifi::new(... Default::default())` → `ModeConfig::Ap(ApConfig { ssid: "reTerminal-setup-XXYY", auth: Open, ... })`.

Open network so the user has nothing to type just to get to the portal.

### DHCP server

**Open research item.** ESP-IDF's softap includes a built-in DHCP server;
need to confirm `esp-radio` exposes that path or whether we need to run our
own DHCP server (there are embassy-net-compatible crates for this). The
captive portal flow doesn't work without DHCP — the client needs an IP and
a gateway/DNS pointer.

### DNS hijack

Tiny UDP server on port 53 that answers every `A` query with the AP's own
IP address (typically `192.168.4.1`). That's what triggers modern phones'
captive-portal detection (Apple / Android / Windows probe known URLs after
joining, see a same-IP response, and pop the portal UI).

### HTTP server

**`picoserve`** — small async web framework, designed for embassy + static
routing. Two routes:

- `GET /` → render the HTML form (SSID text input, password text input, URL
  text input, submit).
- `POST /save` → parse form body, validate, write `Config` to NVS, respond
  with a short "saved, rebooting" HTML, then trigger a reboot after the
  response flushes.

Can add `GET /scan` later to populate a dropdown of visible SSIDs (requires
AP+STA concurrent — see open research).

### QR code on the eInk

- **`qrcodegen`** crate (`no_std` feature), Nayuki's reference implementation
  ported to Rust.
- Encoded payload: `WIFI:T:nopass;S:reTerminal-setup-XXYY;;` — WiFi
  provisioning URI. Phones with QR scanners in the camera app will prompt
  "Join this network?" in one tap.
- Render: compute the module matrix, then drive embedded-graphics'
  `Rectangle`/`Pixel` primitives against the same `error_image`-style
  `Canvas` used for error rendering, scaled up so the QR fits nicely on the
  panel (roughly 8-16 pixels per QR module, centred, with an instructional
  text block underneath giving the AP name in case the QR fails).
- Drawn once at config-mode entry; refreshed with `display_frame_no_wait`
  then the caller waits normally. The eInk holds the image for free while
  the portal runs.

### Exit flow (successful save)

1. Persist the submitted values to NVS (one `set_*` call per field).
2. Flush the HTTP response ("saved, rebooting").
3. Issue a software reset (`esp_hal::system::software_reset()` or
   equivalent — TBD at implementation time).

The next boot's wake reason is `Undefined`, which `determine_wake_action`
maps to `FreshBoot`, which already runs the white pre-flash before
fetching + doing the real refresh. That's the visual confirmation the
device has left config mode; the config-mode code itself doesn't have to
paint anything on exit.

## Staged implementation

Each stage is a self-contained commit that leaves the firmware working.

### Stage 1 — storage layer only

- Add deps: `esp-storage`, `esp-nvs`.
- Small module wrapping the `"config"` NVS namespace with per-key
  `get_wifi_ssid()` / `set_wifi_ssid()` / ... accessors backed directly by
  `esp-nvs` reads/writes. No serialisation crate — each key is either a
  string or a fixed-width integer blob.
- Early in `main()`: call the accessors; for each missing/unreadable value
  fall back to the existing `env!()` string. Thread the resulting values
  into the WiFi setup + URL fetch paths (so they're no longer `const`s but
  plain `&str` / `String` derived from the accessor results).
- No UI changes yet; the `env!()` fallback keeps dev builds working as-is.
- Outcome: firmware functionally identical, but now capable of reading
  config from flash if something else writes it.

### Stage 2 — config-mode trigger + placeholder

- Replace the existing `TODO` placeholder for Previous+Next-held-30 s.
- Add the "no stored config → enter config mode" path.
- The actual body of config mode is still a stub (e.g. just logs "entering
  config mode" and sleeps), but the decision logic and the non-normal
  branch structure are in place.

### Stage 3 — config-mode body (AP + DHCP + DNS + HTTP + QR)

- Bring up the AP, DHCP (if we need our own), DNS hijack, picoserve routes.
- Render the QR code on the panel.
- Form submission writes NVS and triggers exit.
- Keep `blink_task` running through all of it.

### Stage 4 — exit path polish

- On successful save: software reset. The next boot's `FreshBoot` path
  handles the white-pre-flash-plus-fetch visual sequence for free.
- Any save-failure handling (malformed form, NVS write error): show an
  error frame via the existing `error_image::render` path, keep portal
  running.

## Open research items

Before Stage 3, confirm:

1. **Does `esp-radio` ship a DHCP server in AP mode?** If yes, we get
   client IPs for free. If no, pick a DHCP server crate (or write a minimal
   one; DHCP is small).
2. **Does `esp-radio` support AP+STA concurrent?** Needed only if the
   portal should offer a scanned-SSID dropdown; AP-only with manual SSID
   typing is a fine first cut.
3. **`picoserve` vs `edge-http`** — both work on embassy-net; pick based on
   ergonomics once we're actually wiring forms. `picoserve` leans simpler.
4. **QR code sizing** — figure out how many modules fit a ~24-byte payload
   at ECC level M, pick a pixel scale that maximises readability on each
   panel (E1002 is 800×480, E1004 is 1200×1600).

None of these are blocking for Stage 1.

## Risks and unknowns

- **Concurrent WiFi AP + HTTP + DNS + DHCP + eInk driver** is more than
  we've asked of embassy so far in this project; memory pressure and
  executor-task juggling may need tuning.
- **`esp-nvs` docs are sparse (~43%)** and the crate notes "there might be
  some kinks" — if we hit a blocker, fall back to `sequential-storage` is
  relatively painless (just swap the storage layer; the `Config` struct
  stays the same). Keep that escape hatch in mind during Stage 1.
- **Captive portal detection on newer iOS / Android / Windows** varies;
  worst case the user needs to manually open a browser and navigate to
  `http://192.168.4.1/`. The QR code on the panel should include the AP
  URL as a fallback plaintext line.
