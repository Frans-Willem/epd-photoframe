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
use reterminal_e100x::spectra6::Spectra6Color;

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

enum NetworkError {
    ConnectFailed(esp_radio::wifi::WifiError),
    Timeout(Duration),
}

impl NetworkError {
    fn message(&self, ssid: &str) -> String {
        let hint = "To reconfigure, hold Previous+Next for 10 seconds \
                    during the next boot.";
        match self {
            NetworkError::ConnectFailed(e) => format!(
                "WiFi connect failed: {:?}\nSSID: {}\n\n{}",
                e, ssid, hint
            ),
            NetworkError::Timeout(d) => format!(
                "WiFi connect timed out after {} s.\nSSID: {}\n\n{}",
                d.as_secs(),
                ssid,
                hint
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

/// The normal refresh flow: bring the panel up, optionally flash white for
/// immediate feedback, connect to WiFi, fetch + decode the image, trigger
/// the real refresh, then deep-sleep waking on either the 10-minute timer
/// or a button press.
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

    // Chain two stages that each might fail with a user-visible message:
    //  1) wait for the network to come up (bounded by a timeout and a
    //     fast bail-out on auth failure, so wrong creds don't brick the
    //     device), and
    //  2) fetch + decode the image.
    // Any `Err` short-circuits to `error_image::render`, which then goes
    // through the same panel-refresh path as a successful frame.
    let frame_result: Result<Vec<Spectra6Color>, String> = async {
        wait_for_network_ready(net_stack, WIFI_LINK_TIMEOUT)
            .await
            .map_err(|e| e.message(&creds.ssid))?;
        let url: String = match wake_action.query() {
            Some(q) => format!("{}{}", creds.base_url, q),
            None => creds.base_url.clone(),
        };
        println!("Fetching {}", url);
        try_build_frame(net_stack, &url, panel_width, panel_height).await
    }
    .await;

    let frame: Vec<Spectra6Color> = frame_result.unwrap_or_else(|msg| {
        println!("Falling back to error image: {}", msg);
        error_image::render(panel_width, panel_height, &msg)
    });

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
        (&mut gpio_btn_refresh, esp_hal::rtc_cntl::sleep::WakeupLevel::Low),
        (&mut gpio_btn_previous, esp_hal::rtc_cntl::sleep::WakeupLevel::Low),
        (&mut gpio_btn_next, esp_hal::rtc_cntl::sleep::WakeupLevel::Low),
    ];
    let pin_wake_source = esp_hal::rtc_cntl::sleep::RtcioWakeupSource::new(wakeup_pins);

    let timer_wake_source =
        esp_hal::rtc_cntl::sleep::TimerWakeupSource::new(core::time::Duration::from_secs(10 * 60));
    let wake_sources: &[&dyn esp_hal::rtc_cntl::sleep::WakeSource] =
        &[&timer_wake_source, &pin_wake_source];

    println!("Going to deep sleep :)");
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

static NETWORK_RESOURCES: static_cell::ConstStaticCell<embassy_net::StackResources<4>> =
    static_cell::ConstStaticCell::new(embassy_net::StackResources::new());

static RADIO_CONTROLLER: static_cell::StaticCell<esp_radio::Controller> =
    static_cell::StaticCell::new();

use embedded_io_async::BufRead;

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
) -> Result<Vec<Spectra6Color>, String> {
    let dns = embassy_net::dns::DnsSocket::new(stack);
    let tcp_state = embassy_net::tcp::client::TcpClientState::<1, 4096, 4096>::new();
    let tcp = embassy_net::tcp::client::TcpClient::new(stack, &tcp_state);

    println!("Attempting to do HTTP request");
    let mut http_client = reqwless::client::HttpClient::new(&tcp, &dns);
    let mut request = http_client
        .request(reqwless::request::Method::GET, url)
        .await
        .map_err(|e| format!("HTTP request: {:?}", e))?;

    println!("HTTP request done?");
    let mut http_rx_buf = [0u8; 4096];
    let response = request
        .send(&mut http_rx_buf)
        .await
        .map_err(|e| format!("HTTP send: {:?}", e))?;

    let status = response.status;
    let is_text_plain = matches!(
        response.content_type,
        Some(reqwless::headers::ContentType::TextPlain)
    );

    println!("Reading body");
    let mut reader = response.body().reader();
    let mut body: Vec<u8> = Vec::new();
    loop {
        let chunk = reader
            .fill_buf()
            .await
            .map_err(|e| format!("HTTP read: {:?}", e))?;
        if chunk.is_empty() {
            break;
        }
        body.try_reserve(chunk.len())
            .map_err(|e| format!("OOM reading body at {} bytes: {:?}", body.len(), e))?;
        body.extend_from_slice(chunk);
        let n = chunk.len();
        reader.consume(n);
    }
    body.shrink_to_fit();
    println!("Got body ({} bytes)", body.len());

    if !status.is_successful() {
        let body_str = core::str::from_utf8(&body).unwrap_or("<non-utf8 body>");
        return Err(format!("HTTP {}: {}", status.0, body_str));
    }
    if is_text_plain {
        let body_str = core::str::from_utf8(&body).unwrap_or("<non-utf8 body>");
        return Err(format!("Server: {}", body_str));
    }

    println!("Decode PNG");
    let header = minipng::decode_png_header(&body)
        .map_err(|e| format!("PNG header: {:?}", e))?;
    let required = header.required_bytes();
    let mut decode_buf: Vec<u8> = Vec::new();
    decode_buf
        .try_reserve_exact(required)
        .map_err(|e| format!("OOM decode buffer ({} bytes): {:?}", required, e))?;
    decode_buf.resize(required, 0);
    let image = minipng::decode_png(&body, &mut decode_buf)
        .map_err(|e| format!("PNG decode: {:?}", e))?;
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

    let frame_len = panel_width * panel_height;
    let mut frame: Vec<Spectra6Color> = Vec::new();
    frame
        .try_reserve_exact(frame_len)
        .map_err(|e| format!("OOM frame buffer ({} bytes): {:?}", frame_len, e))?;
    for y in 0..panel_height {
        let row_start = y * image.bytes_per_row();
        for x in 0..panel_width {
            frame.push(png_palette[image.pixels()[row_start + x] as usize]);
        }
    }
    Ok(frame)
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

    let wake_action = determine_wake_action(
        wake_reason,
        refresh_latched,
        previous_latched,
        next_latched,
    );

    let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);
    let time_since_boot = rtc.time_since_boot();
    println!(
        "Device booting up - reset={reset_reason:?} wake={wake_reason:?} action={wake_action:?} \
         latched[refresh={refresh_latched} previous={previous_latched} next={next_latched}] \
         held[refresh={refresh_held} previous={previous_held} next={next_held}] \
         uptime={time_since_boot:?}"
    );

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
        config_mode::run(hw, config).await
    }
}
