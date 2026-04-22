#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;

use esp_hal::gpio::{Input, InputConfig, Pin, Pull};
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_println::println;

use esp_hal::spi::Mode as SpiMode;
use esp_hal::spi::master::Config as SpiConfig;
use esp_hal::spi::master::Spi;

use esp_hal::system::SleepSource;

use esp_backtrace as _;

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use reterminal_e100x::config::Config;
use reterminal_e100x::config_mode;
use reterminal_e100x::error_image;
use reterminal_e100x::hardware::{HardwareCtx, WakeAction, WifiCredentials};
use reterminal_e100x::rtc_persisted::RtcPersisted;
use reterminal_e100x::spectra6::Spectra6Color;

// Two URL slots live in RTC-slow RAM so they survive deep sleep:
//
// - `CURRENT_URL` — what's "currently displayed" on the panel. The
//   normal flow reads (doesn't consume) this on every wake and uses
//   it as the base to fetch. Empty on first boot / after a cold
//   power-on, in which case `main_normal` falls back to the NVS
//   `image.url`.
// - `REDIRECT_URL` — a pending redirect from the previous cycle's
//   `Refresh:` header. Only honoured on a Timer wake (server-driven
//   refresh); any other wake cancels it, like a browser drops a
//   meta-refresh timer when the user clicks a link.
//
// `heapless::String` caps the length at compile time so each slot has
// a fixed footprint.
const STORED_URL_MAX: usize = 512;
#[esp_hal::ram(unstable(rtc_slow, persistent))]
static CURRENT_URL: RtcPersisted<heapless::String<STORED_URL_MAX>> = RtcPersisted::new();
#[esp_hal::ram(unstable(rtc_slow, persistent))]
static REDIRECT_URL: RtcPersisted<heapless::String<STORED_URL_MAX>> = RtcPersisted::new();

#[cfg(feature = "e1002")]
use reterminal_e100x::gdep073e01::{self as panel, Gdep073e01};
#[cfg(feature = "e1004")]
use reterminal_e100x::t133a01::{self as panel, T133A01};

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

// Reference RGB for each Spectra 6 ink, used to map PNG palette entries to the
// closest `Spectra6Color`. Same for both Spectra 6 panels.
const PALETTE: [[u8; 3]; 6] = [
    [58, 0, 66],     // Black
    [179, 208, 200], // White
    [215, 233, 0],   // Yellow
    [151, 38, 44],   // Red
    [61, 38, 152],   // Blue
    [96, 104, 86],   // Green
];

const PALETTE_COLORS: [Spectra6Color; 6] = [
    Spectra6Color::Black,
    Spectra6Color::White,
    Spectra6Color::Yellow,
    Spectra6Color::Red,
    Spectra6Color::Blue,
    Spectra6Color::Green,
];

fn color_distance(a: &[u8; 3], b: &[u8; 3]) -> u32 {
    (0..3)
        .map(|i| {
            if a[i] > b[i] {
                a[i] - b[i]
            } else {
                b[i] - a[i]
            }
        })
        .map(|absdiff| (absdiff as u32) * (absdiff as u32))
        .sum()
}

/// Block until the UART0 TX path has fully clocked out everything
/// esp-println handed it. Polls the FIFO byte count down to zero, waits
/// 10 µs for the FSM to transition to idle (the esp-hal driver does the
/// same fixup — the FSM can briefly stay busy after the last byte leaves
/// the FIFO), then polls the "transmit FSM is idle" status bit. Mirrors
/// `esp_hal::uart::UartTx::flush` but operates on the PAC directly so
/// we don't need to steal UART0 from esp-println.
///
/// Used right before `rtc.sleep_deep(..)`: `sleep_deep` cuts the UART
/// peripheral clock, so any bytes still in the shift register when we
/// arrive there are truncated on the wire. esp-println's ROM TX_FLUSH
/// drains the software FIFO but *not* the hardware shift register.
fn wait_for_uart_tx_idle() {
    let uart0 = esp_hal::peripherals::UART0::regs();
    while uart0.status().read().txfifo_cnt().bits() > 0 {}
    esp_hal::delay::Delay::new().delay_micros(10);
    while uart0.fsm_status().read().st_utx_out().bits() != 0 {}
}

/// Read the RTC-IO interrupt status register (the wake latch) masked to the
/// caller's bits of interest, clear those same bits via the register's
/// write-1-to-clear sibling, and return the pre-clear masked value. Other
/// bits in the register are neither read back nor touched.
///
/// Wraps the single `unsafe` the PAC mandates for this register: svd2rust
/// marks `RTC_GPIO_STATUS_W1TC` with `Safety = Unsafe` because it can't
/// statically verify field-value semantics, but writing any `u32` mask to
/// a write-1-to-clear status register only flips hardware status bits in
/// the RTC domain and cannot violate memory safety.
fn read_and_clear_rtc_gpio_wake_status(mask: u32) -> u32 {
    let rtc_io = esp_hal::peripherals::RTC_IO::regs();
    let value = rtc_io.rtc_gpio_status().read().int().bits() & mask;
    unsafe {
        rtc_io
            .rtc_gpio_status_w1tc()
            .write(|w| w.rtc_gpio_status_int_w1tc().bits(mask));
    }
    value
}

fn determine_wake_action(
    wake_reason: SleepSource,
    refresh_latched: bool,
    previous_latched: bool,
    next_latched: bool,
) -> WakeAction {
    match wake_reason {
        SleepSource::Undefined => WakeAction::FreshBoot,
        SleepSource::Timer => WakeAction::Timer,
        // `RtcioWakeupSource` on the ESP32-S3 reports as `Gpio`. The
        // RTC-IO interrupt latch authoritatively tells us which button(s)
        // triggered the wake, even if the user has already released.
        SleepSource::Gpio => {
            if refresh_latched {
                WakeAction::Refresh
            } else if next_latched {
                WakeAction::Next
            } else if previous_latched {
                WakeAction::Previous
            } else {
                println!(
                    "WARNING: Gpio wake with no button bit latched; \
                     defaulting to Refresh"
                );
                WakeAction::Refresh
            }
        }
        other => {
            println!(
                "WARNING: unexpected wake reason {:?}; treating as fresh boot",
                other
            );
            WakeAction::FreshBoot
        }
    }
}

/// Upper bound on how long `main_normal` will wait for the network to
/// come up before giving up and rendering an error frame. Covers the
/// "weak signal / DHCP stall" cases that don't surface as an outright
/// auth error from `connect_async`.
const WIFI_LINK_TIMEOUT: Duration = Duration::from_secs(30);

/// Fallback deep-sleep duration on a successful fetch when the server
/// doesn't send a `Refresh` header. E-ink is fine with hours-scale
/// refreshes and most dashboard content doesn't change faster than
/// that; the server can override on a per-response basis.
const DEFAULT_SUCCESS_SLEEP: embassy_time::Duration = embassy_time::Duration::from_secs(4 * 60 * 60);

/// Deep-sleep duration on the error path. Shorter than success so a
/// transient failure (WiFi glitch, server hiccup) recovers on the next
/// wake without the user having to push a button.
const DEFAULT_ERROR_SLEEP: embassy_time::Duration = embassy_time::Duration::from_secs(600);

enum NetworkError {
    ConnectFailed(esp_radio::wifi::WifiError),
    Timeout(Duration),
}

impl NetworkError {
    fn message(&self, ssid: &str) -> String {
        match self {
            NetworkError::ConnectFailed(e) => {
                format!("WiFi connect failed: {:?}\nSSID: {}", e, ssid)
            }
            NetworkError::Timeout(d) => format!(
                "WiFi connect timed out after {} s.\nSSID: {}",
                d.as_secs(),
                ssid,
            ),
        }
    }
}

/// Wait for the network to get DHCP'd, with both a hard deadline and
/// an early bail-out if `wifi_task` reports a connect failure (wrong
/// password, SSID missing, etc.). Without the race, a bad SSID/password
/// leaves the device hanging forever on `wait_link_up` while the retry
/// loop cycles silently.
async fn wait_for_network_ready(
    stack: embassy_net::Stack<'_>,
    timeout: Duration,
) -> Result<(), NetworkError> {
    use embassy_futures::select::{Either3, select3};
    match select3(
        async {
            stack.wait_link_up().await;
            stack.wait_config_up().await;
        },
        WIFI_CONNECT_FAILURE.wait(),
        Timer::after(timeout),
    )
    .await
    {
        Either3::First(()) => Ok(()),
        Either3::Second(e) => Err(NetworkError::ConnectFailed(e)),
        Either3::Third(()) => Err(NetworkError::Timeout(timeout)),
    }
}

/// The normal refresh flow: connect to WiFi with the stored credentials,
/// fetch + decode the image (or an error frame on failure), trigger the
/// real panel refresh on top of the background white pre-flash that
/// `main()` already kicked off, then deep-sleep waking on either the
/// 10-minute timer or a button press.
async fn main_normal(ctx: HardwareCtx, creds: WifiCredentials) -> ! {
    let HardwareCtx {
        spawner,
        mut rtc,
        wake_action,
        wifi,
        mut gpio_btn_refresh,
        mut gpio_btn_previous,
        mut gpio_btn_next,
        mut spi_bus,
        mut epd,
        mut tft_enable,
    } = ctx;

    let (panel_width, panel_height) = panel::panel_size();

    // --- WiFi bring-up (runs in parallel with any ongoing white pre-flash
    // that `main` started before the config-mode race). ---
    let radio_init = RADIO_CONTROLLER
        .init(esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller"));
    let (mut wifi_controller, interfaces) =
        esp_radio::wifi::new(radio_init, wifi, Default::default())
            .expect("Failed to initialize Wi-Fi controller");

    let wifi_sta_device = interfaces.sta;
    let sta_config = embassy_net::Config::dhcpv4(Default::default());

    let station_config = esp_radio::wifi::ModeConfig::Client(
        esp_radio::wifi::ClientConfig::default()
            .with_ssid(creds.ssid.as_str().into())
            .with_password(creds.password.as_str().into()),
    );
    wifi_controller.set_config(&station_config).unwrap();

    let rng = esp_hal::rng::Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let (net_stack, net_runner) =
        embassy_net::new(wifi_sta_device, sta_config, NETWORK_RESOURCES.take(), seed);

    spawner.spawn(wifi_task(wifi_controller)).unwrap();
    spawner.spawn(net_task(net_runner)).unwrap();

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
        .unwrap_or_else(|| creds.base_url.clone());
    let current_url: String = match wake_action {
        WakeAction::Timer => match REDIRECT_URL.take() {
            Some(r) => {
                println!("Committing pending redirect as current URL: {}", r);
                String::from(r.as_str())
            }
            None => reterminal_e100x::url::strip_query_param(&current_url, "action"),
        },
        other => {
            REDIRECT_URL.clear();
            let stripped = reterminal_e100x::url::strip_query_param(&current_url, "action");
            match other.action_name() {
                Some(name) => {
                    reterminal_e100x::url::append_query_param(&stripped, "action", name)
                }
                None => stripped,
            }
        }
    };
    match heapless::String::<STORED_URL_MAX>::try_from(current_url.as_str()) {
        Ok(s) => CURRENT_URL.set(s),
        Err(()) => {
            println!(
                "Current URL too long ({} bytes) to persist in RTC; NVS fallback after next cold boot",
                current_url.len()
            );
            CURRENT_URL.clear();
        }
    }

    // Two stages that each might fail with a user-visible message:
    //  1) wait for the network to come up (bounded by a timeout and a
    //     fast bail-out on auth failure, so wrong creds don't brick
    //     the device), and
    //  2) fetch + decode the image.
    // Any `Err` short-circuits to `error_image::render`, which then
    // goes through the same panel-refresh path as a successful frame.
    let fetch_result: Result<FetchOutcome, String> = async {
        wait_for_network_ready(net_stack, WIFI_LINK_TIMEOUT)
            .await
            .map_err(|e| e.message(&creds.ssid))?;
        println!("Fetching {}", current_url);
        try_build_frame(net_stack, &current_url, panel_width, panel_height).await
    }
    .await;

    // Success ⇒ use the server's `Refresh` hint if it sent one, else
    // the long-form default. Error ⇒ short retry default so a
    // transient issue gets another shot without the user pressing
    // anything. `wakeup_requested` is an absolute `Instant` so the
    // time we then spend on the panel refresh + UART flush is
    // automatically subtracted from the sleep duration when we
    // compute it below.
    let (frame, wakeup_requested): (_, embassy_time::Instant) = match fetch_result {
        Ok(FetchOutcome { frame, refresh }) => {
            match refresh.as_ref().and_then(|h| h.url_override.as_deref()) {
                // The server often sends relative URLs (e.g. just
                // `/screen/foo`), so resolve against whatever we just
                // fetched rather than the NVS base.
                Some(raw) => match reterminal_e100x::url::resolve(&current_url, raw) {
                    Some(abs) => match heapless::String::<STORED_URL_MAX>::try_from(abs.as_str())
                    {
                        Ok(stored) => {
                            println!("Stashing redirect URL for next Timer wake: {}", abs);
                            REDIRECT_URL.set(stored);
                        }
                        Err(()) => {
                            println!(
                                "Redirect URL too long ({} bytes); dropping",
                                abs.len()
                            );
                            REDIRECT_URL.clear();
                        }
                    },
                    None => {
                        println!("Unresolvable redirect URL {:?}; dropping", raw);
                        REDIRECT_URL.clear();
                    }
                },
                // No pending redirect from the server — leave the slot
                // empty (already cleared on entry for non-Timer wakes;
                // `take()`n on entry for Timer wakes).
                None => {}
            }
            let wakeup = refresh
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
            let wakeup = embassy_time::Instant::now() + DEFAULT_ERROR_SLEEP;
            (error_image::render(panel_width, panel_height, &msg), wakeup)
        }
    };

    // --- Real refresh: reset aborts the (possibly still running) white refresh. ---
    println!("Reset");
    epd.reset(&mut embassy_time::Delay).await.unwrap();
    println!("Wait until idle");
    epd.wait_until_idle().await.unwrap();
    println!("Init");
    epd.init(&mut spi_bus).await.unwrap();
    println!("Power on");
    epd.power_on(&mut spi_bus).await.unwrap();
    println!("Update frame");
    let data = (0..(panel_width * panel_height)).map(|idx| {
        let (x, y) = panel::output_index_to_image_xy(idx);
        frame[y * panel_width + x]
    });
    epd.update_frame(&mut spi_bus, data).await.unwrap();
    println!("Trigger refresh");
    epd.display_frame_no_wait(&mut spi_bus).await.unwrap();
    println!("Wait until idle (~20s refresh)");
    epd.wait_until_idle().await.unwrap();

    println!("Power off");
    epd.power_off(&mut spi_bus).await.unwrap();

    if let Some(ref mut tft) = tft_enable {
        tft.set_low();
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
    let pin_wake_source = esp_hal::rtc_cntl::sleep::RtcioWakeupSource::new(wakeup_pins);

    // Convert the absolute wake-up `Instant` back into a relative
    // duration as late as possible, so any time still spent below
    // (UART flush etc.) doesn't inflate it. `sleep_deep` takes a
    // `core::time::Duration`, so we bridge through micros.
    //
    // Note: the actual sleep duration can still drift by a few percent
    // relative to what we pass here — esp-hal 1.0's
    // `TimerWakeupSource::apply` uses the nominal 150 kHz for the
    // conversion and ignores the calibration it already performed at
    // boot. Fixed upstream in esp-hal 1.1.0 (picked up once the rest
    // of the dependency cascade lands).
    let remaining = wakeup_requested.saturating_duration_since(embassy_time::Instant::now());
    let sleep_duration = core::time::Duration::from_micros(remaining.as_micros());
    println!("Deep sleep for {:?}", sleep_duration);
    let timer_wake_source = esp_hal::rtc_cntl::sleep::TimerWakeupSource::new(sleep_duration);
    let wake_sources: &[&dyn esp_hal::rtc_cntl::sleep::WakeSource] =
        &[&timer_wake_source, &pin_wake_source];

    println!("Going to deep sleep :)");
    wait_for_uart_tx_idle();
    rtc.sleep_deep(wake_sources);
}

#[embassy_executor::task]
async fn blink_task(mut led: Output<'static>) {
    loop {
        led.toggle();
        Timer::after(Duration::from_millis(500)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, esp_radio::wifi::WifiDevice<'static>>) {
    runner.run().await
}

/// Latest `connect_async` failure (if any) from `wifi_task`, surfaced to
/// `main_normal` so it can bail out with an error frame instead of
/// hanging on `net_stack.wait_link_up()`. `main_normal` races this
/// against link-up and a timeout; it only ever takes the signal once
/// per boot, so overwriting on each retry is fine — whichever error is
/// current when main notices wins.
static WIFI_CONNECT_FAILURE: embassy_sync::signal::Signal<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    esp_radio::wifi::WifiError,
> = embassy_sync::signal::Signal::new();

#[embassy_executor::task]
async fn wifi_task(mut controller: esp_radio::wifi::WifiController<'static>) {
    println!("Start connection task");
    println!("Device capabilities: {:?}", controller.capabilities());

    println!("Starting WiFi");
    controller.start_async().await.unwrap();
    println!("Wifi started");
    loop {
        println!("Connecting WiFi");
        match controller.connect_async().await {
            Ok(_) => {
                println!("Connected");
                controller
                    .wait_for_event(esp_radio::wifi::WifiEvent::StaDisconnected)
                    .await;
                println!("Disconnected");
            }
            Err(e) => {
                println!("Failed to connect to wifi: {e:?}");
                WIFI_CONNECT_FAILURE.signal(e);
                println!("Retry in 5sec");
                Timer::after(Duration::from_secs(5)).await;
            }
        }
    }
}

use reterminal_e100x::net_resources::{NETWORK_RESOURCES, RADIO_CONTROLLER};

use embedded_io_async::Read;

/// Parse an `http://host[:port]/path[?query]` URL into its addressable
/// parts. HTTPS isn't supported (we intentionally don't ship embedded-tls)
/// so anything else is rejected up front.
fn parse_http_url(url: &str) -> Result<(&str, u16, &str), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("URL must start with http:// : {}", url))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rfind(':') {
        Some(i) => {
            let p: u16 = authority[i + 1..]
                .parse()
                .map_err(|_| format!("Invalid port in {}", url))?;
            (&authority[..i], p)
        }
        None => (authority, 80u16),
    };
    if host.is_empty() {
        return Err(format!("Empty host in {}", url));
    }
    Ok((host, port, path))
}

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

/// What `try_build_frame` returns on success: the decoded panel frame
/// plus any server-side hint for the next refresh cycle.
struct FetchOutcome {
    frame: Vec<Spectra6Color>,
    refresh: Option<RefreshHint>,
}

/// Parse a `Refresh: <secs>[; url=<url>]` header value into a
/// `(Duration, Option<url>)`. Returns `None` for anything that doesn't
/// start with a non-negative integer. The caller adds the `Duration`
/// to the `Instant` at which the response headers arrived to build a
/// `RefreshHint`.
fn parse_refresh_header(value: &str) -> Option<(embassy_time::Duration, Option<String>)> {
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
        if v.is_empty() { None } else { Some(String::from(v)) }
    });
    Some((embassy_time::Duration::from_secs(interval_secs), url_override))
}

/// Fetch a pre-quantised palette PNG from `url`, decode it, and map its
/// palette entries onto `Spectra6Color`, returning a row-major frame buffer
/// sized to match the panel. On any HTTP / content-type / PNG / sizing
/// failure, returns a human-readable error message; `text/plain` responses
/// are treated as server-side errors and their body is surfaced as the
/// error message.
async fn try_build_frame<'t>(
    stack: embassy_net::Stack<'t>,
    url: &str,
    panel_width: usize,
    panel_height: usize,
) -> Result<FetchOutcome, String> {
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
            .query(host, embassy_net::dns::DnsQueryType::A)
            .await
            .map_err(|e| format!("DNS {}: {:?}", host, e))?;
        let v4 = addrs.iter().find_map(|a| match a {
            embassy_net::IpAddress::Ipv4(v) => Some(*v),
        });
        v4.ok_or_else(|| format!("DNS: no A record for {}", host))?
    };
    let addr = SocketAddr::new(IpAddr::V4(ip), port);
    println!("Resolved {} -> {}", host, addr);

    let tcp_buffers: edge_nal_embassy::TcpBuffers<1, 4096, 4096> =
        edge_nal_embassy::TcpBuffers::new();
    let tcp = edge_nal_embassy::Tcp::new(stack, &tcp_buffers);

    let host_header = if port == 80 {
        String::from(host)
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
        path,
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

    println!("Reading body");
    let mut body: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    {
        let (_, body_reader) = conn.split();
        loop {
            let n = body_reader
                .read(&mut chunk)
                .await
                .map_err(|e| format!("HTTP read: {:?}", e))?;
            if n == 0 {
                break;
            }
            body.try_reserve(n)
                .map_err(|e| format!("OOM reading body at {} bytes: {:?}", body.len(), e))?;
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
        return Err(format!("HTTP {}: {}", status_code, body_str()));
    }
    if !(200..300).contains(&status_code) {
        // Only surface the body as text when the caller plausibly meant
        // it as a message (no Content-Type at all). Binary bodies on an
        // error status are just shown as the code.
        return match ct_base {
            None => Err(format!("HTTP {}: {}", status_code, body_str())),
            Some(_) => Err(format!("HTTP {}", status_code)),
        };
    }
    match ct_base {
        None => {}
        Some(s)
            if s.eq_ignore_ascii_case("image/png")
                || s.eq_ignore_ascii_case("application/octet-stream") => {}
        Some(_) => {
            return Err(format!(
                "Unexpected Content-Type: {}",
                content_type.as_deref().unwrap_or("")
            ));
        }
    }

    println!("Decode PNG");
    let header = minipng::decode_png_header(&body).map_err(|e| format!("PNG header: {:?}", e))?;
    let required = header.required_bytes();
    let mut decode_buf: Vec<u8> = Vec::new();
    decode_buf
        .try_reserve_exact(required)
        .map_err(|e| format!("OOM decode buffer ({} bytes): {:?}", required, e))?;
    decode_buf.resize(required, 0);
    let image =
        minipng::decode_png(&body, &mut decode_buf).map_err(|e| format!("PNG decode: {:?}", e))?;
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

    let png_palette: Vec<Spectra6Color> = (0..=255)
        .map(|index| image.palette(index))
        .map(|rgba| {
            let rgb: [u8; 3] = [rgba[0], rgba[1], rgba[2]];
            let best = PALETTE
                .iter()
                .enumerate()
                .map(|(i, ref_rgb)| (i, color_distance(ref_rgb, &rgb)))
                .reduce(|a, b| if a.1 < b.1 { a } else { b })
                .unwrap();
            PALETTE_COLORS[best.0]
        })
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
    let mut frame: Vec<Spectra6Color> = Vec::new();
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
    Ok(FetchOutcome {
        frame,
        refresh: refresh_hint,
    })
}

/// Extract the palette index for pixel `x` in a row packed at `bit_depth`
/// bits per pixel. PNG packs sub-byte pixels MSB-first — the leftmost
/// pixel lives in the high-order bits of each byte — and `bit_depth` is
/// always a divisor of 8 (1, 2, 4, or 8), so pixels never straddle bytes.
fn palette_index_at(row: &[u8], x: usize, bit_depth: u8) -> u8 {
    let bit_pos = x * bit_depth as usize;
    let byte = row[bit_pos / 8];
    let shift = 8 - bit_depth - (bit_pos % 8) as u8;
    let mask = (1u8 << bit_depth) - 1;
    (byte >> shift) & mask
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let reset_reason = esp_hal::rtc_cntl::reset_reason(esp_hal::system::Cpu::ProCpu);
    let wake_reason = esp_hal::rtc_cntl::wakeup_cause();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Bind semantic button names to the device-specific GPIOs. This is the
    // only place where the silkscreen-to-GPIO mapping appears; everything
    // downstream uses the `gpio_btn_*` handles (and their `.number()`) so
    // the rest of main() is device-agnostic.
    #[cfg(feature = "e1002")]
    let (mut gpio_btn_refresh, mut gpio_btn_previous, mut gpio_btn_next) =
        (peripherals.GPIO3, peripherals.GPIO5, peripherals.GPIO4);
    #[cfg(feature = "e1004")]
    let (mut gpio_btn_refresh, mut gpio_btn_previous, mut gpio_btn_next) =
        (peripherals.GPIO5, peripherals.GPIO4, peripherals.GPIO3);

    // Read all three buttons ASAP so we capture the press even if the user
    // releases quickly after powering the device out of deep sleep.
    let refresh_held = Input::new(
        gpio_btn_refresh.reborrow(),
        InputConfig::default().with_pull(Pull::Up),
    )
    .is_low();
    let previous_held = Input::new(
        gpio_btn_previous.reborrow(),
        InputConfig::default().with_pull(Pull::Up),
    )
    .is_low();
    let next_held = Input::new(
        gpio_btn_next.reborrow(),
        InputConfig::default().with_pull(Pull::Up),
    )
    .is_low();

    // Snapshot the RTC-IO wake latch (which pin actually triggered the wake)
    // and clear it immediately so stale bits don't carry into the next cycle.
    // The latch is authoritative because it captures the pin state at the
    // exact moment of wake — even a sub-millisecond tap is recorded, whereas
    // the current-level read above misses anything released during the
    // ~300 ms bootloader window.
    let button_bits = (1u32 << gpio_btn_refresh.number())
        | (1u32 << gpio_btn_previous.number())
        | (1u32 << gpio_btn_next.number());
    let rtc_gpio_int_mask = read_and_clear_rtc_gpio_wake_status(button_bits);

    let refresh_latched = (rtc_gpio_int_mask & (1u32 << gpio_btn_refresh.number())) != 0;
    let previous_latched = (rtc_gpio_int_mask & (1u32 << gpio_btn_previous.number())) != 0;
    let next_latched = (rtc_gpio_int_mask & (1u32 << gpio_btn_next.number())) != 0;

    let wake_action =
        determine_wake_action(wake_reason, refresh_latched, previous_latched, next_latched);

    let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);
    let time_since_boot = rtc.time_since_boot();
    println!(
        "Device booting up - reset={reset_reason:?} wake={wake_reason:?} action={wake_action:?} \
         latched[refresh={refresh_latched} previous={previous_latched} next={next_latched}] \
         held[refresh={refresh_held} previous={previous_held} next={next_held}] \
         uptime={time_since_boot:?}"
    );
    println!("RTC CURRENT_URL: {:?}", CURRENT_URL.get().as_deref());
    println!("RTC REDIRECT_URL: {:?}", REDIRECT_URL.get().as_deref());

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    // Load runtime configuration from NVS. A fresh / blank partition is
    // not an error (esp-nvs treats all-0xFF as "no entries yet"), so the
    // only ways `Config::new` fails are programming bugs (wrong partition
    // offset/size) or actual flash hardware trouble — panicking there is
    // the right call. A missing *key* is different: that's how we detect
    // "needs configuring" and short-circuit into config mode below.
    let mut config =
        Config::new(peripherals.FLASH).expect("NVS init failed — check partition table and flash");
    let creds = match (
        config.wifi_ssid().ok().flatten(),
        config.wifi_password().ok().flatten(),
        config.image_url().ok().flatten(),
    ) {
        (Some(ssid), Some(password), Some(base_url)) => {
            println!(
                "Config in use: wifi.ssid={:?} wifi.pass=<{} chars> image.url={:?}",
                ssid,
                password.len(),
                base_url
            );
            Some(WifiCredentials {
                ssid,
                password,
                base_url,
            })
        }
        _ => {
            println!("NVS config incomplete; forcing config mode");
            None
        }
    };

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    spawner
        .spawn(blink_task(Output::new(
            peripherals.GPIO6,
            Level::Low,
            OutputConfig::default(),
        )))
        .unwrap();

    // --- Build the panel SPI bus and EPD driver (shared by both flows) ---
    let epd_spi_bus = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_write_bit_order(esp_hal::spi::BitOrder::MsbFirst)
            .with_frequency(esp_hal::time::Rate::from_mhz(20))
            .with_mode(SpiMode::_0),
    )
    .unwrap();

    #[cfg(feature = "e1002")]
    let mut epd_spi_bus = epd_spi_bus
        .with_sck(peripherals.GPIO7)
        .with_mosi(peripherals.GPIO9)
        .into_async();

    #[cfg(feature = "e1004")]
    let mut epd_spi_bus = epd_spi_bus
        .with_sck(peripherals.GPIO7)
        .with_miso(peripherals.GPIO8)
        .with_mosi(peripherals.GPIO9)
        .into_async();

    // E1004: TFT enable rail must be high while the panel is powered.
    #[cfg(feature = "e1004")]
    let tft_enable: Option<Output<'static>> = Some({
        let mut p = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
        p.set_high();
        p
    });
    #[cfg(feature = "e1002")]
    let tft_enable: Option<Output<'static>> = None;

    #[cfg(feature = "e1002")]
    let mut epd = Gdep073e01::new(
        &mut epd_spi_bus,
        Output::new(peripherals.GPIO20, Level::Low, OutputConfig::default()),
        Input::new(
            peripherals.GPIO13,
            InputConfig::default().with_pull(Pull::Up),
        ),
        Output::new(peripherals.GPIO11, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default()),
        &mut embassy_time::Delay,
    );

    #[cfg(feature = "e1004")]
    let mut epd = T133A01::new(
        &mut epd_spi_bus,
        Output::new(peripherals.GPIO10, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO2, Level::Low, OutputConfig::default()),
        Input::new(
            peripherals.GPIO13,
            InputConfig::default().with_pull(Pull::Up),
        ),
        Output::new(peripherals.GPIO11, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO38, Level::Low, OutputConfig::default()),
        &mut embassy_time::Delay,
    );

    // --- White pre-flash (non-blocking) for immediate user feedback ---
    //
    // Kicked off before we decide which flow to run so the user sees the
    // panel start updating as soon as the device wakes, even while the
    // config-mode race is still counting down. Whichever flow wins will
    // reset the panel to draw its own content on top; the ~20 s refresh
    // just continues in the background until then.
    if wake_action.show_white_flash() {
        let (panel_width, panel_height) = panel::panel_size();
        println!("White pre-flash");
        println!("Reset");
        epd.reset(&mut embassy_time::Delay).await.unwrap();
        println!("Wait until idle");
        epd.wait_until_idle().await.unwrap();
        println!("Init");
        epd.init(&mut epd_spi_bus).await.unwrap();
        println!("Power on");
        epd.power_on(&mut epd_spi_bus).await.unwrap();
        println!("Update frame (white)");
        epd.update_frame(
            &mut epd_spi_bus,
            (0..(panel_width * panel_height)).map(|_| Spectra6Color::White),
        )
        .await
        .unwrap();
        println!("Trigger refresh (no wait)");
        epd.display_frame_no_wait(&mut epd_spi_bus).await.unwrap();
    }

    // If NVS didn't produce a full set of credentials we go straight to
    // config mode without the button race. Otherwise: race a 10-second
    // timer against either Previous or Next being released — if both were
    // held at boot and stay held for the whole window, the timer wins and
    // we enter configuration mode. If either (or both) was never pressed,
    // `wait_for_high` resolves immediately because the pin is already high,
    // so normally this block completes in microseconds.
    let entering_config_mode = creds.is_none() || {
        let mut prev_input = Input::new(
            gpio_btn_previous.reborrow(),
            InputConfig::default().with_pull(Pull::Up),
        );
        let mut next_input = Input::new(
            gpio_btn_next.reborrow(),
            InputConfig::default().with_pull(Pull::Up),
        );
        matches!(
            embassy_futures::select::select3(
                prev_input.wait_for_high(),
                next_input.wait_for_high(),
                Timer::after(Duration::from_secs(10)),
            )
            .await,
            embassy_futures::select::Either3::Third(_)
        )
    };

    let hw = HardwareCtx {
        spawner,
        rtc,
        wake_action,
        wifi: peripherals.WIFI,
        gpio_btn_refresh: gpio_btn_refresh.degrade(),
        gpio_btn_previous: gpio_btn_previous.degrade(),
        gpio_btn_next: gpio_btn_next.degrade(),
        spi_bus: epd_spi_bus,
        epd,
        tft_enable,
    };

    if let Some(creds) = creds
        && !entering_config_mode
    {
        main_normal(hw, creds).await
    } else {
        // Entering config mode is a deliberate reset of the device's
        // "what's displayed" state — whatever URL was committed last
        // cycle (and any pending redirect) is no longer relevant once
        // the user has reconfigured.
        CURRENT_URL.clear();
        REDIRECT_URL.clear();
        config_mode::run(hw, config).await
    }
}
