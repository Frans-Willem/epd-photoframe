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

use esp_hal::gpio::{Input, InputConfig, InputPin, Pull};
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_println::println;

use esp_hal::spi::Mode as SpiMode;
use esp_hal::spi::master::Config as SpiConfig;
use esp_hal::spi::master::Spi;

use embedded_hal_bus::spi::ExclusiveDevice;

use esp_backtrace as _;

extern crate alloc;

use reterminal_e100x::gdep073e01::Gdep073e01State;
use reterminal_e100x::spectra6::Spectra6Color;

use nalgebra::base::Vector6;
use nalgebra::geometry::Point3;
//use epd_dither::decomposer6c::{Decomposer6C, Decomposer6CAxisStrategy};
//use epd_dither::noise::interleaved_gradient_noise;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

const PALETTE: [Point3<f32>; 6] = [
    // Black
    Point3::new(
        0.22676264646610847 * 255.0,
        0.0 * 255.0, //-0.0055970314385675474,
        0.2597094644681131 * 255.0,
    ),
    // White
    Point3::new(
        0.7000095212021477 * 255.0,
        0.8161663966432444 * 255.0,
        0.7861384978213591 * 255.0,
    ),
    // NOTE: Next 4 need to be ordered cyclically!
    // Blue
    Point3::new(
        0.2391826586300411 * 255.0,
        0.1482584219935382 * 255.0,
        0.596627604515917 * 255.0,
    ),
    // Green
    Point3::new(
        0.37804726446284537 * 255.0,
        0.40898865257247025 * 255.0,
        0.3388335156024263 * 255.0,
    ),
    // Red
    Point3::new(
        0.5903086439496402 * 255.0,
        0.14710309178681208 * 255.0,
        0.17200386219219121 * 255.0,
    ),
    // Yellow
    Point3::new(
        0.8417257856614314 * 255.0,
        0.9126861145185275 * 255.0,
        0.0 * 255.0, //-0.053016650312371474,
    ),
];

const PALETTE_COLORS: [Spectra6Color; 6] = [
    Spectra6Color::Black,
    Spectra6Color::White,
    Spectra6Color::Blue,
    Spectra6Color::Green,
    Spectra6Color::Red,
    Spectra6Color::Yellow,
];

fn color_to_point(color: [u8; 4]) -> Point3<f32> {
    let [r, g, b, _] = color.map(|c| c as f32);
    Point3::new(r, g, b)
}

// TODO: Move into epd-dither
fn pick_from_barycentric_weights(weights: Vector6<f32>, offset: f32) -> usize {
    let mut index = 0;
    let mut offset = offset;
    while index + 1 < 6 && weights[index] < offset {
        offset -= weights[index];
        index += 1;
    }
    index
}

struct Button<'t> {
    input: Input<'t>,
    inverted: bool,
}

impl<'t> Button<'t> {
    pub fn new(pin: impl InputPin + 't, config: InputConfig, inverted: bool) -> Self {
        Button {
            input: Input::new(pin, config),
            inverted,
        }
    }
    pub fn is_pressed(&self) -> bool {
        if self.inverted {
            self.input.is_low()
        } else {
            self.input.is_high()
        }
    }

    pub fn is_released(&self) -> bool {
        !self.is_pressed()
    }

    pub async fn wait_for(&mut self, pressed: bool) {
        if self.inverted ^ pressed {
            self.input.wait_for_high().await
        } else {
            self.input.wait_for_low().await
        }
    }

    pub async fn wait_for_pressed(&mut self) {
        self.wait_for(true).await
    }

    pub async fn wait_for_released(&mut self) {
        self.wait_for(false).await
    }
}

#[embassy_executor::task(pool_size = 3)]
async fn button_task(mut button: Button<'static>, button_name: &'static str) {
    loop {
        loop {
            button.wait_for_pressed().await;
            Timer::after(Duration::from_millis(10)).await; // debounce
            if button.is_pressed() {
                break;
            }
        }
        println!("Button {0} pressed!", button_name);
        loop {
            button.wait_for_released().await;
            Timer::after(Duration::from_millis(10)).await; // debounce
            if button.is_released() {
                break;
            }
        }
        println!("Button {0} released!", button_name);
    }
}

#[embassy_executor::task]
async fn blink_task(mut led: Output<'static>) {
    loop {
        //println!("Toggle LED!");
        led.toggle();
        Timer::after(Duration::from_millis(500)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, esp_radio::wifi::WifiDevice<'static>>) {
    runner.run().await
}

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
async fn get_image_data<'t>(stack: embassy_net::Stack<'t>) -> alloc::vec::Vec<u8> {
    // DNS Client
    let dns = embassy_net::dns::DnsSocket::new(stack);
    // TCP state
    let tcp_state = embassy_net::tcp::client::TcpClientState::<1, 4096, 4096>::new();
    let tcp = embassy_net::tcp::client::TcpClient::new(stack, &tcp_state);

    println!("Attempting to do HTTP request");
    let mut http_client = reqwless::client::HttpClient::new(&tcp, &dns);
    const URL: &str = env!("WIFI_URL");
    let mut request = http_client
        .request(reqwless::request::Method::GET, URL)
        .await
        .unwrap();
    println!("HTTP request done?");
    let mut http_rx_buf = [0u8; 4096];
    let mut response = request
        .send(&mut http_rx_buf)
        .await
        .unwrap()
        .body()
        .reader();
    println!("Reading body");

    let mut body = alloc::vec::Vec::new();
    loop {
        let chunk = response.fill_buf().await.unwrap();
        if chunk.is_empty() {
            break;
        }
        body.extend_from_slice(chunk);
        let len = chunk.len();
        response.consume(len);
    }
    println!("Got body");
    body
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let reset_reason = esp_hal::rtc_cntl::reset_reason(esp_hal::system::Cpu::ProCpu);
    let wake_reason = esp_hal::rtc_cntl::wakeup_cause();
    // generator version: 1.0.1
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let mut gpio_btn_reset = peripherals.GPIO3;
    let btn_reset_state = esp_hal::gpio::Input::new(
        gpio_btn_reset.reborrow(),
        esp_hal::gpio::InputConfig::default().with_pull(Pull::Up),
    )
    .is_low();
    let mut rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);

    let time_since_boot = rtc.time_since_boot();

    println!(
        "Device booting up - {reset_reason:?} - {wake_reason:?} - {btn_reset_state:?} - {time_since_boot:?}"
    );

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    /*
    spawner
        .spawn(button_task(
            Button::new(
                peripherals.GPIO3,
                InputConfig::default().with_pull(Pull::Up),
                true,
            )
            "Refresh",
        ))
        .unwrap();
    */
    spawner
        .spawn(button_task(
            Button::new(
                peripherals.GPIO4,
                InputConfig::default().with_pull(Pull::Up),
                true,
            ),
            "Right",
        ))
        .unwrap();
    spawner
        .spawn(button_task(
            Button::new(
                peripherals.GPIO5,
                InputConfig::default().with_pull(Pull::Up),
                true,
            ),
            "Left",
        ))
        .unwrap();
    spawner
        .spawn(blink_task(Output::new(
            peripherals.GPIO6,
            Level::Low,
            OutputConfig::default(),
        )))
        .unwrap();

    let radio_init = RADIO_CONTROLLER
        .init(esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller"));
    let (mut wifi_controller, interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");

    const SSID: &str = env!("WIFI_SSID");
    const PASSWORD: &str = env!("WIFI_PASSWORD");

    let wifi_sta_device = interfaces.sta;

    let sta_config = embassy_net::Config::dhcpv4(Default::default());

    let station_config = esp_radio::wifi::ModeConfig::Client(
        esp_radio::wifi::ClientConfig::default()
            .with_ssid(SSID.into())
            .with_password(PASSWORD.into()),
    );
    wifi_controller.set_config(&station_config).unwrap();

    let rng = esp_hal::rng::Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let (net_stack, net_runner) =
        embassy_net::new(wifi_sta_device, sta_config, NETWORK_RESOURCES.take(), seed);

    spawner.spawn(wifi_task(wifi_controller)).unwrap();
    spawner.spawn(net_task(net_runner)).unwrap();

    println!("Waiting for network link...");
    net_stack.wait_link_up().await;
    println!("Link up, waiting for config up");
    net_stack.wait_config_up().await;
    println!("Network config up! {:?}", net_stack.config_v4());

    let png_data = get_image_data(net_stack).await;
    println!("Decode PNG");
    let (header, data) = png_decoder::decode(png_data.as_slice()).unwrap();
    println!("Header: {:?}", header);
    let data = data.into_iter();

    let epd_spi_bus = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_write_bit_order(esp_hal::spi::BitOrder::MsbFirst)
            .with_frequency(esp_hal::time::Rate::from_mhz(20))
            .with_mode(SpiMode::_0),
    )
    .unwrap();
    let epd_spi_bus = epd_spi_bus
        .with_sck(peripherals.GPIO7)
        .with_mosi(peripherals.GPIO9)
        .into_async();

    let mut epd_spi_dev = ExclusiveDevice::new(
        epd_spi_bus,
        Output::new(peripherals.GPIO20, Level::Low, OutputConfig::default()),
        embassy_time::Delay,
    )
    .unwrap();

    let epd = Gdep073e01State::new(
        &mut epd_spi_dev,
        Input::new(
            peripherals.GPIO13,
            InputConfig::default().with_pull(Pull::Up),
        ),
        Output::new(peripherals.GPIO11, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default()),
        &mut embassy_time::Delay,
    );

    /*
    println!("Creating decomposer");
    let decomposer = Decomposer6C::new(&PALETTE).unwrap();

    println!("Setting up dithering iterator");
    let data = data.map(color_to_point);
    // let data = data.map(|x| x * 0.8);
    let data = data.enumerate().map(|(index, color)| {
        let barycentric: Vector6<f32> = decomposer.decompose(&color, Decomposer6CAxisStrategy::Closest);
        let x = index % 800;
        let y = index / 800;
        let noise = interleaved_gradient_noise(x as f32, y as f32);
        let index = pick_from_barycentric_weights(barycentric, noise);
        PALETTE_COLORS[index].clone()
    });

    println!("Dithering");
    let start_dither = esp_hal::xtensa_lx::timer::get_cycle_count();
    let data: alloc::vec::Vec<Spectra6Color> = data.collect();
    let end_dither = esp_hal::xtensa_lx::timer::get_cycle_count();
    let dither_duration_cycles = end_dither.wrapping_sub(start_dither);
    println!("Duration: {:?} seconds", (dither_duration_cycles as f32)/(240_000_000.0));
    */
    let data = data.map(|color| {
        let color_pt = color_to_point(color);
        let distances = PALETTE.iter().enumerate().map(|(index, pt)| (index, (pt.clone() - color_pt).norm_squared()));
        let best = distances.reduce(|a,b| {
            if a.1 < b.1 {
                a
            } else {
                b
            }
        }).unwrap();
        PALETTE_COLORS[best.0]
    });

    println!("Reset");
    let epd = epd.reset(&mut embassy_time::Delay).await.unwrap();
    println!("Init");
    let epd = epd.init(&mut epd_spi_dev).await.unwrap();
    println!("Power on");
    let epd = epd.power_on(&mut epd_spi_dev).await.unwrap();
    println!("Update frame");
    let epd = epd.update_frame(&mut epd_spi_dev, data).await.unwrap();
    println!("Display frame");
    let epd = epd.display_frame(&mut epd_spi_dev).await.unwrap();
    // Quick hack to allow clearing the screen for storage:
    let epd = if esp_hal::gpio::Input::new(
        gpio_btn_reset.reborrow(),
        esp_hal::gpio::InputConfig::default().with_pull(Pull::Up),
    )
    .is_low()
    {
        println!("Clearing screen before power off");
        let epd = epd
            .update_frame(
                &mut epd_spi_dev,
                (0..(800 * 480)).map(|_| Spectra6Color::Clean),
            )
            .await
            .unwrap();
        epd.display_frame(&mut epd_spi_dev).await.unwrap()
    } else {
        epd
    };

    println!("Power off");
    let epd = epd.power_off(&mut epd_spi_dev).await.unwrap();
    // TODO: Display deep sleep
    println!("Done");
    let _ = epd;

    // TODO: Spawn some tasks
    let _ = spawner;

    println!("Deep sleep!");

    let wakeup_pins: &mut [(
        &mut dyn esp_hal::gpio::RtcPin,
        esp_hal::rtc_cntl::sleep::WakeupLevel,
    )] = &mut [(
        &mut gpio_btn_reset,
        esp_hal::rtc_cntl::sleep::WakeupLevel::Low,
    )];
    let pin_wake_source = esp_hal::rtc_cntl::sleep::RtcioWakeupSource::new(wakeup_pins);

    let timer_wake_source =
        esp_hal::rtc_cntl::sleep::TimerWakeupSource::new(core::time::Duration::from_secs(10 * 60));
    let wake_sources: &[&dyn esp_hal::rtc_cntl::sleep::WakeSource] =
        &[&timer_wake_source, &pin_wake_source];

    println!("Going to deep sleep :)");
    rtc.sleep_deep(wake_sources);
}
