//! Configuration mode: bring up a WiFi AP (open, SSID suffix derived from
//! the AP MAC), run a DHCP server so joined phones actually get an IP,
//! run a DNS hijack so captive-portal probes land on our own IP, and
//! render a QR code + textual instructions on the panel. Stage 3c will
//! layer an HTTP captive-portal form on top of the same AP and trigger a
//! software reset on save.

use alloc::format;

use embassy_net::Stack;
use embassy_time::{Duration, Timer};
use esp_println::println;

use crate::config::Config;
use crate::config_image;
use crate::hardware::HardwareCtx;
use crate::portal;

#[cfg(feature = "e1002")]
use crate::gdep073e01 as panel;
#[cfg(feature = "e1004")]
use crate::t133a01 as panel;

/// Per-mode static resources — kept local to this module so the normal
/// flow can have its own without cross-referencing. Only one of the two
/// modes runs per boot, so duplicating them in `.bss` is cheap.
// Socket budget: 1 DHCP + 1 DNS + up to `web_task`'s handler-task pool
// (4) of live HTTP connections, plus slack so probe bursts don't panic.
// Each slot is small (smoltcp socket metadata only — the TCP rx/tx rings
// live in the separate `TcpBuffers` pool), so 10 costs us on the order
// of ~1 KB extra RAM.
static NETWORK_RESOURCES: static_cell::ConstStaticCell<embassy_net::StackResources<10>> =
    static_cell::ConstStaticCell::new(embassy_net::StackResources::new());
static RADIO_CONTROLLER: static_cell::StaticCell<esp_radio::Controller> =
    static_cell::StaticCell::new();

pub async fn run(ctx: HardwareCtx, mut nvs: Config<'static>) -> ! {
    let HardwareCtx {
        spawner,
        wifi,
        mut spi_bus,
        mut epd,
        mut tft_enable,
        ..
    } = ctx;
    let (panel_width, panel_height) = panel::panel_size();

    // --- Bring up WiFi in AP mode. The AP MAC is stable per-device, so we
    // derive a user-recognisable SSID suffix from its last two bytes. ---
    let radio_init = RADIO_CONTROLLER
        .init(esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller"));
    let (mut wifi_controller, interfaces) =
        esp_radio::wifi::new(radio_init, wifi, Default::default())
            .expect("Failed to initialize Wi-Fi controller");
    let ap_device = interfaces.ap;

    let ap_mac = esp_radio::wifi::ap_mac();
    let ap_ssid = format!("reTerminal-setup-{:02X}{:02X}", ap_mac[4], ap_mac[5]);
    println!("AP SSID: {}", ap_ssid);

    let ap_mode_config = esp_radio::wifi::ModeConfig::AccessPoint(
        esp_radio::wifi::AccessPointConfig::default()
            .with_ssid(ap_ssid.clone())
            .with_auth_method(esp_radio::wifi::AuthMethod::None),
    );
    wifi_controller.set_config(&ap_mode_config).unwrap();

    // --- Static embassy-net stack on the AP interface. 192.168.4.1/24 is
    // the ESP-IDF softap convention; keeping it here means the portal URL
    // is predictable for users who miss the QR scan. ---
    let ap_addr = core::net::Ipv4Addr::new(192, 168, 4, 1);
    let mut dns_servers = heapless::Vec::new();
    let _ = dns_servers.push(ap_addr);
    let static_config = embassy_net::StaticConfigV4 {
        address: embassy_net::Ipv4Cidr::new(ap_addr, 24),
        gateway: Some(ap_addr),
        dns_servers,
    };
    let net_config = embassy_net::Config::ipv4_static(static_config);

    let rng = esp_hal::rng::Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let (net_stack, net_runner) =
        embassy_net::new(ap_device, net_config, NETWORK_RESOURCES.take(), seed);

    spawner.spawn(ap_wifi_task(wifi_controller)).unwrap();
    spawner.spawn(net_task(net_runner)).unwrap();
    spawner.spawn(dhcp_server_task(net_stack)).unwrap();
    spawner.spawn(dns_hijack_task(net_stack)).unwrap();
    spawner.spawn(portal::web_task(net_stack)).unwrap();

    // --- Panel content: QR + instructions. The QR encodes a WiFi-join URI
    // so most phones' camera-app scanners offer a one-tap "Join network"
    // action; the text below it gives the SSID (for manual entry) and the
    // portal URL (for users whose phones don't surface the captive-portal
    // popup yet — Stage 3c). ---
    let payload = format!("WIFI:T:nopass;S:{};;", ap_ssid);
    let instructions = format!(
        "reTerminal Setup\n\n\
         Scan the QR code, or\n\
         join WiFi:\n\
         {}\n\n\
         Then open:\n\
         http://192.168.4.1/",
        ap_ssid
    );
    println!("Rendering config screen with QR: {}", payload);
    let frame = config_image::render(panel_width, panel_height, &payload, &instructions);

    println!("Reset");
    epd.reset(&mut embassy_time::Delay).await.unwrap();
    println!("Wait until idle");
    epd.wait_until_idle().await.unwrap();
    println!("Init");
    epd.init(&mut spi_bus).await.unwrap();
    println!("Power on");
    epd.power_on(&mut spi_bus).await.unwrap();
    println!("Update frame (QR)");
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

    // Wait for the portal to hand us a credentials blob, then write it
    // to NVS and reset. A failure to write is logged but we reset anyway
    // — on the next boot either the new values land (partial success)
    // or we re-enter config mode (NVS still incomplete) and the user
    // retries. Either way, we don't want to be stuck in a black hole
    // after the user pressed "Save".
    println!("Config mode ready — awaiting save from the HTTP portal.");
    let creds = portal::SAVE_SIGNAL.wait().await;
    println!(
        "Saving config to NVS: ssid={:?}, url={:?}",
        creds.ssid, creds.url
    );
    if let Err(e) = nvs.set_wifi_ssid(&creds.ssid) {
        println!("WARNING: failed to write wifi.ssid: {:?}", e);
    }
    if let Err(e) = nvs.set_wifi_password(&creds.password) {
        println!("WARNING: failed to write wifi.pass: {:?}", e);
    }
    if let Err(e) = nvs.set_image_url(&creds.url) {
        println!("WARNING: failed to write image.url: {:?}", e);
    }

    // Give the HTTP response a moment to flush before we reboot.
    Timer::after(Duration::from_millis(500)).await;

    println!("Rebooting to apply new config");
    esp_hal::system::software_reset();
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, esp_radio::wifi::WifiDevice<'static>>) {
    runner.run().await
}

/// Start the WiFi controller in AP mode and keep it alive. `start_async`
/// internally waits for `WifiEvent::ApStart`, so by the time it returns
/// the AP is advertising and accepting associations. The controller must
/// not be dropped while we want the AP to stay up, hence the idle loop.
#[embassy_executor::task]
async fn ap_wifi_task(mut controller: esp_radio::wifi::WifiController<'static>) {
    println!("Starting WiFi in AP mode");
    controller.start_async().await.unwrap();
    println!("AP started");
    loop {
        // Log association events as they come in; they're useful for
        // verifying a phone actually joined the portal network.
        controller
            .wait_for_event(esp_radio::wifi::WifiEvent::ApStaConnected)
            .await;
        println!("AP: station connected");
    }
}

/// Serve DHCP on the AP interface so phones joining the captive-portal
/// network get an IP address immediately (otherwise modern phones time out
/// the join and drop the network as "no internet"). `run` loops forever.
#[embassy_executor::task]
async fn dhcp_server_task(stack: Stack<'static>) {
    let mut server = leasehund::DhcpServer::<8, 1>::new_with_dns(
        core::net::Ipv4Addr::new(192, 168, 4, 1),   // server IP
        core::net::Ipv4Addr::new(255, 255, 255, 0), // subnet mask
        core::net::Ipv4Addr::new(192, 168, 4, 1),   // gateway
        core::net::Ipv4Addr::new(192, 168, 4, 1),   // DNS (self; see dns_hijack_task)
        core::net::Ipv4Addr::new(192, 168, 4, 100), // pool start
        core::net::Ipv4Addr::new(192, 168, 4, 200), // pool end
    );
    server.run(stack).await
}

/// Captive-portal DNS hijack: reply to every `A` query with the AP's own
/// IP address. That's what makes iOS/Android/Windows pop the "sign in to
/// this network" prompt automatically after they join.
#[embassy_executor::task]
async fn dns_hijack_task(stack: Stack<'static>) {
    use core::net::{IpAddr, Ipv4Addr, SocketAddr};

    let ap_ip = Ipv4Addr::new(192, 168, 4, 1);
    let buffers: edge_nal_embassy::UdpBuffers<1, 512, 512, 2> =
        edge_nal_embassy::UdpBuffers::new();
    let udp = edge_nal_embassy::Udp::new(stack, &buffers);
    let mut tx = [0u8; 512];
    let mut rx = [0u8; 512];
    loop {
        let err = edge_captive::io::run(
            &udp,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 53),
            &mut tx,
            &mut rx,
            ap_ip,
            core::time::Duration::from_secs(60),
        )
        .await;
        println!("DNS hijack exited ({:?}); restarting", err);
        Timer::after(Duration::from_millis(500)).await;
    }
}
