//! Normal-flow boot path: connect to WiFi with the stored credentials,
//! fetch + decode the image (or render an error frame on failure),
//! drive the real panel refresh, and deep-sleep waking on either the
//! `Refresh:` interval / fallback timer or a button press.
//!
//! Mirrors [`crate::config_mode`] and [`crate::panic_mode`] in shape:
//! a single [`run`] entry point that consumes the [`AppContext`] and
//! never returns.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use embassy_time::Duration;
use embedded_io_async::Read;
use esp_hal::gpio::{Input, InputConfig, Output, Pull};
use esp_println::println;

use crate::app::{AppContext, WakeAction};
use crate::battery;
use crate::button::wait_for_press;
use crate::config::Config;
use crate::error_image;
use crate::panel::{Panel, PanelColor};
use crate::rtc_persisted::RtcPersisted;
use crate::sht40;
use crate::single_shot_wifi;
use crate::sy6974b;
#[cfg(feature = "power_measurement")]
use crate::text_box;
use crate::uart::wait_for_tx_idle;
use crate::url_util::{self, parse_http_url};

/// Two URL slots live in RTC-slow RAM so they survive deep sleep:
///
/// - [`CURRENT_URL`] — what's "currently displayed" on the panel. The
///   normal flow reads (doesn't consume) this on every wake and uses
///   it as the base to fetch. Empty on first boot / after a cold
///   power-on, in which case [`run`] falls back to the NVS
///   `image.url`.
/// - [`REDIRECT_URL`] — a pending redirect from the previous cycle's
///   `Refresh:` header. Only honoured on a Timer wake (server-driven
///   refresh); any other wake cancels it, like a browser drops a
///   meta-refresh timer when the user clicks a link.
///
/// `heapless::String` caps the length at compile time so each slot has
/// a fixed footprint.
const STORED_URL_MAX: usize = 512;
#[esp_hal::ram(unstable(rtc_slow, persistent))]
pub static CURRENT_URL: RtcPersisted<heapless::String<STORED_URL_MAX>> = RtcPersisted::new();
#[esp_hal::ram(unstable(rtc_slow, persistent))]
pub static REDIRECT_URL: RtcPersisted<heapless::String<STORED_URL_MAX>> = RtcPersisted::new();

/// When a button is pressed *during* the panel refresh wait, we abort
/// the refresh, stash the resulting action here, and software-reset.
/// On the next boot, this slot overrides the wake-reason-derived
/// action, so the user gets exactly the action they pressed even
/// though we didn't go through deep-sleep + RTC-IO wake. RTC-slow RAM
/// survives software reset (the RTC domain isn't reset) but is zero
/// on cold boot — `RtcPersisted`'s magic gate handles that.
#[esp_hal::ram(unstable(rtc_slow, persistent))]
pub static PENDING_ACTION: RtcPersisted<WakeAction> = RtcPersisted::new();

/// Battery voltage in millivolts. Set once per boot by `main`'s
/// sensor task and consumed by [`run`].
pub static BATTERY_MV: embassy_sync::signal::Signal<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    u16,
> = embassy_sync::signal::Signal::new();

/// Temperature + relative humidity from the SHT40. `None` if the
/// read failed. Set once per boot by the sensor task.
pub static TEMP_HUMIDITY: embassy_sync::signal::Signal<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    Option<sht40::TempHumidity>,
> = embassy_sync::signal::Signal::new();

/// SY6974B charger state — battery / charging / full / fault. E1004
/// only; on E1002 / E1001 (different charger IC, no I²C address shared
/// with the UC8179 / Spectra controllers) this stays `None` and the
/// URL is built without a `power=` param. Set once per boot by the
/// sensor task.
pub static POWER_STATUS: embassy_sync::signal::Signal<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    Option<sy6974b::PowerStatus>,
> = embassy_sync::signal::Signal::new();

struct FrameWithPalette<C> {
    frame: Vec<C>,
    palette: Vec<C>,
}

/// Sensor task. Spawned at boot, signals into the [`BATTERY_MV`] /
/// [`TEMP_HUMIDITY`] / [`POWER_STATUS`] slots. Battery (ADC + GPIO21)
/// is independent of I²C0, so it runs in parallel with the I²C-bound
/// chain. Inside the chain, reads share the bus sequentially: SHT40
/// first, then the SY6974B charger on boards where that chip is
/// accessible to this firmware.
#[embassy_executor::task]
pub async fn sensor_task(
    battery_enable: Output<'static>,
    adc1: esp_hal::peripherals::ADC1<'static>,
    battery_sense: esp_hal::peripherals::GPIO1<'static>,
    mut i2c0: esp_hal::i2c::master::I2c<'static, esp_hal::Async>,
    has_sy6974b: bool,
) {
    let i2c_reads = async {
        let temp_humidity = sht40::read_temp_humidity(&mut i2c0).await;
        let power_status = if has_sy6974b {
            sy6974b::read_power_status(&mut i2c0).await
        } else {
            None
        };
        (temp_humidity, power_status)
    };
    let (battery_mv, (temp_humidity, power_status)) = embassy_futures::join::join(
        battery::read_battery(battery_enable, adc1, battery_sense),
        i2c_reads,
    )
    .await;
    BATTERY_MV.signal(battery_mv);
    TEMP_HUMIDITY.signal(temp_humidity);
    POWER_STATUS.signal(power_status);
}

/// Upper bound on how long [`run`] will wait for the network to come
/// up before giving up and rendering an error frame. Covers the "weak
/// signal / DHCP stall" cases that don't surface as an outright auth
/// error from `connect_async`.
const WIFI_LINK_TIMEOUT: Duration = Duration::from_secs(30);

/// Fallback deep-sleep duration on a successful fetch when the server
/// doesn't send a `Refresh` header. E-ink is fine with hours-scale
/// refreshes and most dashboard content doesn't change faster than
/// that; the server can override on a per-response basis.
const DEFAULT_SUCCESS_SLEEP: Duration = Duration::from_secs(4 * 60 * 60);

/// Deep-sleep duration on the error path. Shorter than success so a
/// transient failure (WiFi glitch, server hiccup) recovers on the next
/// wake without the user having to push a button.
const DEFAULT_ERROR_SLEEP: Duration = Duration::from_secs(600);
#[cfg(feature = "power_measurement")]
const POWER_MEASUREMENT_SLEEP: Duration = Duration::from_secs(60);

/// Optional server hint parsed from the `Refresh:` response header —
/// carries the target `Instant` at which the next fetch should happen
/// (computed from the server's interval plus whenever the response
/// headers arrived) and an optional one-shot URL override for that
/// fetch. Using an absolute `Instant` instead of a raw interval means
/// the time we spend on the panel refresh and the UART flush between
/// "headers received" and `sleep_deep` is automatically subtracted,
/// so the next wake lines up with the server's intended cadence.
#[derive(Debug, Clone)]
struct RefreshHint {
    refresh_time: embassy_time::Instant,
    url_override: Option<String>,
}

/// Error path from `try_fetch` (and its downstream decode pipeline).
/// Early failures (DNS, TCP, request send) carry `refresh: None` since
/// no headers were received. Failures after headers were parsed —
/// non-2xx status, unexpected Content-Type, body-read errors — carry
/// the parsed `Refresh:` hint so the error path can honour the
/// server's retry interval the same way the success path does.
#[derive(Debug)]
struct FetchError {
    message: String,
    refresh: Option<RefreshHint>,
}

impl From<String> for FetchError {
    fn from(message: String) -> Self {
        Self {
            message,
            refresh: None,
        }
    }
}

/// The normal refresh flow. Connect to WiFi with the stored
/// credentials, fetch + decode the image (or an error frame on
/// failure), trigger the real panel refresh on top of the background
/// white pre-flash that `main()`'s `run_normal_boot` may have kicked
/// off, then deep-sleep waking on either the `Refresh:` interval (or
/// the fallback timer) or a button press.
pub async fn run<P>(ctx: AppContext<P>, mut config: Config<'static>) -> !
where
    P: Panel<esp_hal::spi::master::Spi<'static, esp_hal::Async>>,
{
    let AppContext {
        spawner,
        rtc,
        wake_action,
        wifi,
        has_sy6974b,
        battery_enable,
        adc1,
        battery_sense,
        i2c0,
        mut gpio_btn_refresh,
        mut gpio_btn_previous,
        mut gpio_btn_next,
        mut spi_bus,
        mut epd,
        ..
    } = ctx;

    let (panel_width, panel_height) = (P::WIDTH, P::HEIGHT);

    println!("RTC CURRENT_URL: {:?}", CURRENT_URL.get().as_deref());
    println!("RTC REDIRECT_URL: {:?}", REDIRECT_URL.get().as_deref());

    // By the time the URL is being built below, both sensor signals
    // should be populated — the 10 ms ADC settle + the 10 ms SHT40
    // conversion happen alongside the ~1.3 s WiFi association, so the
    // `wait()`s in the fetch closure are essentially free.
    spawner.spawn(sensor_task(battery_enable, adc1, battery_sense, i2c0, has_sy6974b).unwrap());

    // Figure out what to fetch this cycle. Start from whatever's in
    // RTC (defaulting to the NVS URL on cold boot) and adjust per the
    // wake reason:
    //
    // - Timer wake: take the pending redirect if any, else keep the
    //   current URL minus any leftover `action=` from a previous
    //   button wake.
    // - Any other wake (FreshBoot / Refresh / Previous / Next):
    //   cancel any pending redirect like browser navigation cancels
    //   a meta-refresh, strip the old `action=`, and re-append a
    //   fresh one for button wakes.
    //
    // The result is written back to `CURRENT_URL` so the display's
    // "current state" in RTC always matches what we actually fetched.
    let current_url: String = CURRENT_URL
        .get()
        .map(|s| String::from(s.as_str()))
        .unwrap_or_else(|| config.get_image_url().ok().flatten().unwrap_or_default());
    let current_url: String = match wake_action {
        WakeAction::Timer => match REDIRECT_URL.take() {
            Some(r) => {
                println!("Committing pending redirect as current URL: {}", r);
                String::from(r.as_str())
            }
            None => url_util::set_query_variable(&current_url, "action", None),
        },
        other => {
            REDIRECT_URL.clear();
            url_util::set_query_variable(&current_url, "action", other.action_name())
        }
    };
    match heapless::String::<STORED_URL_MAX>::try_from(current_url.as_str()) {
        Ok(s) => CURRENT_URL.set(s),
        Err(heapless::CapacityError { .. }) => {
            println!(
                "Current URL too long ({} bytes) to persist in RTC; NVS fallback after next cold boot",
                current_url.len()
            );
            CURRENT_URL.clear();
        }
    }

    // Bring WiFi up just long enough to do the HTTP fetch, then let
    // `single_shot_wifi::run` tear it all down so PNG decoding runs
    // with the radio off. PNG decoding is CPU-bound and expensive
    // (minipng's decode buffer is the biggest transient allocation
    // in the app), so keeping WiFi off for it saves the RF duty cycle.
    //
    // Sensor params (battery_mv, battery_pct, …) live on the fetch
    // URL only, *not* on the persisted `current_url` — next wake's
    // readings should be fresh. Awaiting `BATTERY_MV` here, inside
    // the `single_shot_wifi::run` closure, is on purpose: the battery
    // task was spawned at boot and runs concurrently with WiFi
    // association (~1.3 s), so by the time WiFi is up and the closure
    // fires the 10 ms ADC read has long since signaled and `wait()`
    // returns instantly. Awaiting it before WiFi started would
    // serialise the two and waste that overlap.
    let fetch_result: Result<(Vec<u8>, Option<RefreshHint>), FetchError> = {
        let base_url_ref = current_url.as_str();
        let wifi_result =
            single_shot_wifi::run(wifi, &mut config, WIFI_LINK_TIMEOUT, |stack| async move {
                let battery_mv = BATTERY_MV.wait().await;
                let battery_pct = battery::mv_to_percentage(battery_mv);
                let temp_humidity = TEMP_HUMIDITY.wait().await;
                let power_status = POWER_STATUS.wait().await;
                let fetch_url = url_util::set_query_variable(
                    base_url_ref,
                    "battery_mv",
                    Some(&format!("{}", battery_mv)),
                );
                let fetch_url = url_util::set_query_variable(
                    &fetch_url,
                    "battery_pct",
                    Some(&format!("{}", battery_pct)),
                );
                let fetch_url = if let Some(th) = temp_humidity {
                    let url = url_util::set_query_variable(
                        &fetch_url,
                        "temp_c",
                        Some(&format!("{:.2}", th.temperature_c)),
                    );
                    url_util::set_query_variable(
                        &url,
                        "humidity_pct",
                        Some(&format!("{:.2}", th.humidity_pct)),
                    )
                } else {
                    fetch_url
                };
                let fetch_url = if let Some(ps) = power_status {
                    url_util::set_query_variable(&fetch_url, "power", Some(ps.as_str()))
                } else {
                    fetch_url
                };
                println!("Fetching {}", fetch_url);
                try_fetch(stack, &fetch_url).await
            })
            .await;
        match wifi_result {
            Ok(inner) => inner,
            Err(e) => Err(FetchError::from(
                e.message(&config.get_wifi_ssid().ok().flatten().unwrap_or_default()),
            )),
        }
    };

    // WiFi is now off. Decode (and palette-quantise) the PNG body.
    // Preserve the server's Refresh hint on decode errors so the
    // error path can still honour it. `try_decode_frame` returns
    // both the row-major frame and the actual PNG palette (sized to
    // `1 << bit_depth`) so the caller can pick a panel init mode.
    let frame_result: Result<FrameWithPalette<P::Color>, String>;
    let hint: Option<RefreshHint>;
    match fetch_result {
        Ok((body, h)) => {
            hint = h;
            frame_result = try_decode_frame(&body, panel_width, panel_height);
        }
        Err(FetchError { message, refresh }) => {
            hint = refresh;
            frame_result = Err(message);
        }
    }

    // Success ⇒ use the server's `Refresh` hint if it sent one, else
    // the long-form default. Error ⇒ also honour the hint if the
    // server attached one to the error response, else fall back to
    // the short retry default so a transient issue gets another shot
    // without the user pressing anything. `wakeup_requested` is an
    // absolute `Instant` so the time we then spend on the panel refresh
    // + UART flush is automatically subtracted from the sleep duration
    // when we compute it below.
    let (frame, wakeup_requested): (FrameWithPalette<P::Color>, embassy_time::Instant) =
        match frame_result {
            Ok(frame) => {
                if let Some(raw) = hint.as_ref().and_then(|h| h.url_override.as_deref()) {
                    // The server often sends relative URLs (e.g. just
                    // `/screen/foo`), so resolve against whatever we just
                    // fetched rather than the NVS base.
                    match url_util::resolve(&current_url, raw) {
                        Some(abs) => {
                            match heapless::String::<STORED_URL_MAX>::try_from(abs.as_str()) {
                                Ok(stored) => {
                                    println!("Stashing redirect URL for next Timer wake: {}", abs);
                                    REDIRECT_URL.set(stored);
                                }
                                Err(heapless::CapacityError { .. }) => {
                                    println!(
                                        "Redirect URL too long ({} bytes); dropping",
                                        abs.len()
                                    );
                                    REDIRECT_URL.clear();
                                }
                            }
                        }
                        None => {
                            println!("Unresolvable redirect URL {:?}; dropping", raw);
                            REDIRECT_URL.clear();
                        }
                    }
                }
                let wakeup = hint
                    .map(|h| h.refresh_time)
                    .unwrap_or_else(|| embassy_time::Instant::now() + DEFAULT_SUCCESS_SLEEP);
                (frame, wakeup)
            }
            Err(msg) => {
                println!("Falling back to error image: {}", msg);
                // Clear CURRENT_URL so the next wake falls back to the NVS
                // base URL — if the stored current URL is what caused the
                // error (e.g. a bad redirect committed last cycle), a
                // transient retry against the same URL would just loop.
                CURRENT_URL.clear();
                let wakeup = hint
                    .map(|h| h.refresh_time)
                    .unwrap_or_else(|| embassy_time::Instant::now() + DEFAULT_ERROR_SLEEP);
                let retry_in = wakeup.saturating_duration_since(embassy_time::Instant::now());
                // The error image is rendered with `BLACK` and `WHITE` only,
                // so its effective palette is exactly those two colours.
                (
                    FrameWithPalette {
                        frame: error_image::render(panel_width, panel_height, &msg, Some(retry_in)),
                        palette: Vec::from([P::Color::BLACK, P::Color::WHITE]),
                    },
                    wakeup,
                )
            }
        };
    #[cfg(feature = "power_measurement")]
    let wakeup_requested = {
        let _server_wakeup_requested = wakeup_requested;
        println!("Power measurement mode: forcing 60 second wake interval");
        embassy_time::Instant::now() + POWER_MEASUREMENT_SLEEP
    };

    #[cfg(feature = "power_measurement")]
    let frame = add_power_measurement_overlay(frame, panel_width, panel_height);

    let FrameWithPalette { frame, palette } = frame;

    // --- Real refresh: reset aborts the (possibly still running) white refresh. ---
    //
    // The whole panel sequence — reset → init → power_on →
    // update_frame → display_frame_no_wait → wait_until_idle →
    // power_off → disable — is wrapped in an async block and raced
    // against the three buttons. A press during *any* of those steps
    // (not just the long wait in the middle) aborts the panel via a
    // fresh `reset()`, stashes the chosen action in RTC-slow RAM, and
    // software-resets the device. The next boot's
    // `determine_wake_action` picks up `PENDING_ACTION` and resumes
    // with that action, the same way it would for a deep-sleep wake.
    // Pick the cheapest init mode that covers the frame. On E1001 this
    // chooses B/W vs 4-level grayscale based on the colours actually
    // present in the PNG palette (≈ half the SPI traffic + faster
    // waveform when the server-side content is already 1bpp).
    // Single-mode panels return `()` immediately without iterating.
    let init_mode = P::init_mode_for_palette(palette.iter().copied());
    let panel_update = async {
        // Bring the panel's enable rail up before any panel I/O.
        // Idempotent — fine if the white pre-flash already raised it.
        epd.enable().await.unwrap();
        println!("Reset");
        epd.reset().await.unwrap();
        println!("Wait until idle");
        epd.wait_until_idle().await.unwrap();
        println!("Init");
        epd.init(&mut spi_bus, init_mode).await.unwrap();
        println!("Power on");
        epd.power_on(&mut spi_bus).await.unwrap();
        println!("Update frame");
        let data = (0..(panel_width * panel_height)).map(|idx| {
            let (x, y) = P::output_index_to_image_xy(idx);
            frame[y * panel_width + x]
        });
        epd.update_frame(&mut spi_bus, data).await.unwrap();
        println!("Trigger refresh");
        epd.display_frame_no_wait(&mut spi_bus).await.unwrap();
        println!("Wait until idle (~20s refresh)");
        epd.wait_until_idle().await.unwrap();
        println!("Power off");
        epd.power_off(&mut spi_bus).await.unwrap();
    };

    const BUTTON_DEBOUNCE: Duration = Duration::from_millis(50);
    let interrupted: Option<WakeAction> = {
        let mut refresh_in = Input::new(
            gpio_btn_refresh.reborrow(),
            InputConfig::default().with_pull(Pull::Up),
        );
        let mut previous_in = Input::new(
            gpio_btn_previous.reborrow(),
            InputConfig::default().with_pull(Pull::Up),
        );
        let mut next_in = Input::new(
            gpio_btn_next.reborrow(),
            InputConfig::default().with_pull(Pull::Up),
        );
        use embassy_futures::select::{Either4, select4};
        match select4(
            panel_update,
            wait_for_press(&mut refresh_in, BUTTON_DEBOUNCE),
            wait_for_press(&mut previous_in, BUTTON_DEBOUNCE),
            wait_for_press(&mut next_in, BUTTON_DEBOUNCE),
        )
        .await
        {
            Either4::First(()) => None,
            Either4::Second(()) => Some(WakeAction::Refresh),
            Either4::Third(()) => Some(WakeAction::Previous),
            Either4::Fourth(()) => Some(WakeAction::Next),
        }
    };
    // Always de-assert the power to the panel, either update completed or interrupted
    epd.disable().await.unwrap();

    if let Some(action) = interrupted {
        println!(
            "Button {:?} pressed during refresh; aborting + soft-resetting",
            action
        );
        epd.reset().await.unwrap();
        PENDING_ACTION.set(action);
        wait_for_tx_idle();
        esp_hal::system::software_reset();
    }

    println!("Done");
    let _ = epd;

    println!("Deep sleep!");
    let wakeup_pins: &mut [(
        &mut dyn esp_hal::gpio::RtcPin,
        esp_hal::rtc_cntl::sleep::WakeupLevel,
    )] = &mut [
        (
            &mut gpio_btn_refresh,
            esp_hal::rtc_cntl::sleep::WakeupLevel::Low,
        ),
        (
            &mut gpio_btn_previous,
            esp_hal::rtc_cntl::sleep::WakeupLevel::Low,
        ),
        (
            &mut gpio_btn_next,
            esp_hal::rtc_cntl::sleep::WakeupLevel::Low,
        ),
    ];
    crate::sleep::start_sleep(rtc, Some(wakeup_requested), wakeup_pins);
}

#[cfg(feature = "power_measurement")]
fn add_power_measurement_overlay<C: PanelColor>(
    frame: FrameWithPalette<C>,
    width: usize,
    height: usize,
) -> FrameWithPalette<C> {
    const TEXT: &str =
        "Power measurement mode\nCharger disabled if supported\nWake interval: 60 seconds";

    let FrameWithPalette { frame, mut palette } = frame;
    let frame = text_box::draw_centered_on_frame(frame, width as u32, height as u32, TEXT);
    add_color_if_missing(&mut palette, C::BLACK);
    add_color_if_missing(&mut palette, C::WHITE);
    FrameWithPalette { frame, palette }
}

#[cfg(feature = "power_measurement")]
fn add_color_if_missing<C: PanelColor>(palette: &mut Vec<C>, color: C) {
    if !palette.contains(&color) {
        palette.push(color);
    }
}

/// Parse a `Refresh: <secs>[; url=<url>]` header value into a
/// `(Duration, Option<url>)`. Returns `None` for anything that doesn't
/// start with a non-negative integer. The caller adds the `Duration`
/// to the `Instant` at which the response headers arrived to build a
/// `RefreshHint`.
fn parse_refresh_header(value: &str) -> Option<(Duration, Option<String>)> {
    let value = value.trim();
    let (secs_part, rest) = match value.split_once(';') {
        Some((a, b)) => (a.trim(), Some(b.trim())),
        None => (value, None),
    };
    let interval_secs: u64 = secs_part.parse().ok()?;
    let url_override = rest.and_then(|r| {
        let (key, raw) = r.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("url") {
            return None;
        }
        let v = raw.trim();
        // Strip matching surrounding quotes if present.
        let v = if let Some(inner) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            inner
        } else {
            v
        };
        if v.is_empty() {
            None
        } else {
            Some(String::from(v))
        }
    });
    Some((Duration::from_secs(interval_secs), url_override))
}

/// Fetch the body of `url` over HTTP and parse the `Refresh:` header
/// if present. Returns the raw body bytes (expected to be PNG; the
/// content-type classification checks that) plus a `RefreshHint`
/// synthesised to fall back to `DEFAULT_SUCCESS_SLEEP` when the
/// server didn't send a `Refresh`. `text/plain` response bodies are
/// treated as server-side error messages regardless of status code.
async fn try_fetch<'t>(
    stack: embassy_net::Stack<'t>,
    url: &str,
) -> Result<(Vec<u8>, Option<RefreshHint>), FetchError> {
    use core::net::{IpAddr, Ipv4Addr, SocketAddr};

    let (host, port, path) = parse_http_url(url)?;

    // Resolve the host to an IPv4 address. IP-literal hosts skip the DNS
    // round-trip; everything else goes through embassy-net's resolver
    // (which is seeded by the DHCP server option on connect).
    let ip: Ipv4Addr = if let Ok(ip) = host.parse::<Ipv4Addr>() {
        ip
    } else {
        let dns = embassy_net::dns::DnsSocket::new(stack);
        let addrs = dns
            .query(&host, embassy_net::dns::DnsQueryType::A)
            .await
            .map_err(|e| format!("DNS {}: {:?}", host, e))?;
        let v4 = addrs
            .iter()
            .map(|a| match a {
                embassy_net::IpAddress::Ipv4(v) => *v,
            })
            .next();
        v4.ok_or_else(|| format!("DNS: no A record for {}", host))?
    };
    let addr = SocketAddr::new(IpAddr::V4(ip), port);
    println!("Resolved {} -> {}", host, addr);

    let tcp_buffers: edge_nal_embassy::TcpBuffers<1, 4096, 4096> =
        edge_nal_embassy::TcpBuffers::new();
    let tcp = edge_nal_embassy::Tcp::new(stack, &tcp_buffers);

    let host_header = if port == 80 {
        host.clone()
    } else {
        format!("{}:{}", host, port)
    };
    let mut http_buf = [0u8; 4096];
    let mut conn: edge_http::io::client::Connection<_, 32> =
        edge_http::io::client::Connection::new(&mut http_buf, &tcp, addr);

    println!("Attempting to do HTTP request");
    conn.initiate_request(
        true,
        edge_http::Method::Get,
        &path,
        &[("Host", host_header.as_str()), ("Connection", "close")],
    )
    .await
    .map_err(|e| format!("HTTP request: {:?}", e))?;
    conn.initiate_response()
        .await
        .map_err(|e| format!("HTTP send: {:?}", e))?;
    // Anchor the refresh target as close as possible to the moment
    // the server's response arrived. Everything between here and
    // `sleep_deep` (body read, PNG decode, panel refresh, UART flush)
    // then counts against the server's interval rather than adding to
    // it.
    let headers_received_at = embassy_time::Instant::now();

    // Snapshot the status + Content-Type + any `Refresh:` hint before
    // we release the header borrow and start reading the body. The
    // full Content-Type (with any `; charset=...` parameters) is kept
    // verbatim for error messages; we'll normalise to a base type for
    // classification below.
    let (status_code, content_type, refresh_hint) = {
        let headers = conn
            .headers()
            .map_err(|e| format!("HTTP headers: {:?}", e))?;
        let refresh = headers.headers.get("Refresh").and_then(|v| {
            parse_refresh_header(v).map(|(interval, url_override)| {
                println!(
                    "Server Refresh interval: {} s from headers-received",
                    interval.as_secs()
                );
                RefreshHint {
                    refresh_time: headers_received_at + interval,
                    url_override,
                }
            })
        });
        (
            headers.code,
            headers.headers.content_type().map(String::from),
            refresh,
        )
    };

    // From here on errors carry the parsed `Refresh:` hint so the
    // caller can honour the server's retry interval on failures too.
    let with_refresh_hint = |message: String| FetchError {
        message,
        refresh: refresh_hint.clone(),
    };

    println!("Reading body");
    let mut body: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    {
        let (_, body_reader) = conn.split();
        loop {
            let n = body_reader
                .read(&mut chunk)
                .await
                .map_err(|e| with_refresh_hint(format!("HTTP read: {:?}", e)))?;
            if n == 0 {
                break;
            }
            body.try_reserve(n).map_err(|e| {
                with_refresh_hint(format!("OOM reading body at {} bytes: {:?}", body.len(), e))
            })?;
            body.extend_from_slice(&chunk[..n]);
        }
    }
    body.shrink_to_fit();
    println!("Got body ({} bytes)", body.len());

    // Classify: the server can hand us an error message as `text/plain`
    // (even with a 200), a PNG payload as `image/png` /
    // `application/octet-stream` / no Content-Type, or something else
    // we don't know what to do with.
    let ct_base = content_type
        .as_deref()
        .map(|s| s.split(';').next().unwrap_or("").trim());
    let body_str = || core::str::from_utf8(&body).unwrap_or("<non-utf8 body>");

    if ct_base.is_some_and(|s| s.eq_ignore_ascii_case("text/plain")) {
        // text/plain is always an error surface, regardless of status code.
        return Err(with_refresh_hint(format!(
            "HTTP {}: {}",
            status_code,
            body_str()
        )));
    }
    if !(200..300).contains(&status_code) {
        // Only surface the body as text when the caller plausibly meant
        // it as a message (no Content-Type at all). Binary bodies on an
        // error status are just shown as the code.
        return Err(match ct_base {
            None => with_refresh_hint(format!("HTTP {}: {}", status_code, body_str())),
            Some(_) => with_refresh_hint(format!("HTTP {}", status_code)),
        });
    }
    match ct_base {
        None => {}
        Some(s)
            if s.eq_ignore_ascii_case("image/png")
                || s.eq_ignore_ascii_case("application/octet-stream") => {}
        Some(_) => {
            return Err(with_refresh_hint(format!(
                "Unexpected Content-Type: {}",
                content_type.as_deref().unwrap_or("")
            )));
        }
    }

    Ok((body, refresh_hint))
}

/// Decode a pre-quantised indexed PNG into a row-major frame buffer of
/// the panel's colour type, sized to match the panel. Split out of
/// `try_fetch` because the `minipng` decode buffer and palette lookup
/// are by far the biggest transient allocations in the fetch cycle, and
/// we run with WiFi off while they're in flight.
///
/// Returns a [`FrameWithPalette`] containing the row-major frame and PNG
/// palette. The palette holds exactly the `1 << bit_depth` real PNG
/// palette entries (2 for 1bpp, 4 for 2bpp, …) — the caller passes it
/// to [`Panel::init_mode_for_palette`] to pick the cheapest panel init
/// mode that covers the content.
fn try_decode_frame<C: PanelColor>(
    body: &[u8],
    panel_width: usize,
    panel_height: usize,
) -> Result<FrameWithPalette<C>, String> {
    use embedded_graphics::pixelcolor::Rgb888;
    println!("Decode PNG");
    let header = minipng::decode_png_header(body).map_err(|e| format!("PNG header: {:?}", e))?;
    let required = header.required_bytes();
    let mut decode_buf: Vec<u8> = Vec::new();
    decode_buf
        .try_reserve_exact(required)
        .map_err(|e| format!("OOM decode buffer ({} bytes): {:?}", required, e))?;
    decode_buf.resize(required, 0);
    let image =
        minipng::decode_png(body, &mut decode_buf).map_err(|e| format!("PNG decode: {:?}", e))?;
    println!("Decoded PNG");
    println!(
        "Image: {}x{} {:?} {:?}",
        image.width(),
        image.height(),
        image.color_type(),
        image.bit_depth()
    );

    if image.width() as usize != panel_width || image.height() as usize != panel_height {
        return Err(format!(
            "Image {}x{} does not match panel {}x{}",
            image.width(),
            image.height(),
            panel_width,
            panel_height
        ));
    }
    if image.color_type() != minipng::ColorType::Indexed {
        return Err(format!(
            "Unsupported PNG color type: {:?} (need Indexed)",
            image.color_type()
        ));
    }
    let bit_depth = match image.bit_depth() {
        minipng::BitDepth::One => 1u8,
        minipng::BitDepth::Two => 2,
        minipng::BitDepth::Four => 4,
        minipng::BitDepth::Eight => 8,
        other => return Err(format!("Unsupported PNG bit depth: {:?}", other)),
    };

    // Build the palette → panel-colour lookup. Read only the
    // `1 << bit_depth` real PNG palette entries (2 for 1bpp, 4 for
    // 2bpp, …) — `from_rgb` is paid once per entry, not per pixel.
    let palette_len = 1usize << bit_depth;
    let png_palette: Vec<C> = (0..palette_len)
        .map(|index| image.palette(index as u8))
        .map(|rgba| C::from_rgb(Rgb888::new(rgba[0], rgba[1], rgba[2])))
        .collect();

    // Validate the decoded pixel buffer is self-consistent before indexing
    // into it — otherwise a short / truncated row from a corrupt PNG would
    // panic inside the inner loop.
    let bytes_per_row = image.bytes_per_row();
    let pixels = image.pixels();
    let min_bytes_per_row = (panel_width * bit_depth as usize).div_ceil(8);
    if bytes_per_row < min_bytes_per_row {
        return Err(format!(
            "PNG row too short: {} bytes/row but need at least {} for {} {}-bpp pixels",
            bytes_per_row, min_bytes_per_row, panel_width, bit_depth
        ));
    }
    let expected_len = bytes_per_row.checked_mul(panel_height).ok_or_else(|| {
        format!(
            "Image dimensions overflow: {} x {}",
            bytes_per_row, panel_height
        )
    })?;
    if pixels.len() < expected_len {
        return Err(format!(
            "PNG pixel buffer too small: {} bytes, expected at least {} \
             ({} bytes/row × {} rows)",
            pixels.len(),
            expected_len,
            bytes_per_row,
            panel_height
        ));
    }

    let frame_len = panel_width * panel_height;
    let mut frame: Vec<C> = Vec::new();
    frame
        .try_reserve_exact(frame_len)
        .map_err(|e| format!("OOM frame buffer ({} bytes): {:?}", frame_len, e))?;
    for y in 0..panel_height {
        let row = &pixels[y * bytes_per_row..(y + 1) * bytes_per_row];
        for x in 0..panel_width {
            let palette_index = palette_index_at(row, x, bit_depth);
            frame.push(png_palette[palette_index as usize]);
        }
    }
    Ok(FrameWithPalette {
        frame,
        palette: png_palette,
    })
}

/// Extract the palette index for pixel `x` in a row packed at `bit_depth`
/// bits per pixel. PNG packs sub-byte pixels MSB-first — the leftmost
/// pixel lives in the high-order bits of each byte — and `bit_depth` is
/// always a divisor of 8 (1, 2, 4, or 8), so pixels never straddle bytes.
/// The mask is computed in `u16` so `bit_depth == 8` doesn't overflow
/// `1u8 << 8`.
fn palette_index_at(row: &[u8], x: usize, bit_depth: u8) -> u8 {
    let bit_pos = x * bit_depth as usize;
    let byte = row[bit_pos / 8];
    let shift = 8 - bit_depth - (bit_pos % 8) as u8;
    let mask = ((1u16 << bit_depth) - 1) as u8;
    (byte >> shift) & mask
}
