//! Configuration mode: bring up a WiFi AP (open, SSID suffix derived
//! from the AP MAC), run a DHCP server so joined phones actually get
//! an IP, run a DNS hijack so captive-portal probes land on our own
//! IP, serve an HTTP form from `portal.rs` for the user to submit
//! WiFi credentials and image URL, and render a QR code + textual
//! instructions on the panel. On form submission, persist the values
//! to NVS and trigger a software reset back into the normal flow.

use alloc::format;
use alloc::string::String;

use embassy_net::Stack;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::spi::master::Spi;
use esp_println::println;

use crate::button::wait_for_press;
use crate::config::Config;
use crate::config_image;
use crate::hardware::{EpdColor, EpdPanel, HardwareCtx};
use crate::net_resources::NETWORK_RESOURCES;
use crate::panel::{Panel, PanelColor};

mod portal;

pub async fn run(ctx: HardwareCtx, mut nvs: Config<'static>) -> ! {
    let HardwareCtx {
        spawner,
        wifi,
        gpio_btn_refresh,
        spi_bus,
        epd,
        mut buzzer,
        ..
    } = ctx;

    // Audible "you can release the buttons now" cue. Fires before any
    // panel work so the user gets feedback within ~80 ms of the
    // 10-second hold completing, well before the QR + instructions
    // appear on the panel a few seconds later.
    println!("Config mode — beep");
    buzzer.beep(Duration::from_millis(80)).await;

    // Current NVS values pre-fill the portal form so the user can edit
    // one field without re-typing the others. A non-empty stored
    // password gives the form a "keep existing" sentinel; otherwise
    // (brand-new device or prior open-network config) the field starts
    // empty and whatever the user submits gets saved as-is.
    let stored_ssid = nvs.wifi_ssid().ok().flatten().unwrap_or_default();
    let stored_url = nvs.image_url().ok().flatten().unwrap_or_default();
    let password_is_set = nvs
        .wifi_password()
        .ok()
        .flatten()
        .is_some_and(|p| !p.is_empty());

    // --- Bring up WiFi in AP mode. The AP MAC is stable per-device, so we
    // derive a user-recognisable SSID suffix from its last two bytes.
    // Constructing the controller with `initial_config` configures and
    // starts the radio; dropping it stops it. ---
    let ap_mac =
        esp_hal::efuse::interface_mac_address(esp_hal::efuse::InterfaceMacAddress::AccessPoint);
    let ap_mac_bytes = ap_mac.as_bytes();
    let ap_ssid = format!(
        "epd-photoframe-setup-{:02X}{:02X}",
        ap_mac_bytes[4], ap_mac_bytes[5]
    );
    println!("AP SSID: {}", ap_ssid);

    let ap_mode_config = esp_radio::wifi::Config::AccessPoint(
        esp_radio::wifi::ap::AccessPointConfig::default()
            .with_ssid(ap_ssid.as_str())
            .with_auth_method(esp_radio::wifi::AuthenticationMethod::None),
    );
    let controller_config =
        esp_radio::wifi::ControllerConfig::default().with_initial_config(ap_mode_config);
    let (wifi_controller, interfaces) = esp_radio::wifi::new(wifi, controller_config)
        .expect("Failed to initialize Wi-Fi controller");
    let ap_device = interfaces.access_point;

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

    spawner.spawn(ap_wifi_task(wifi_controller).unwrap());
    spawner.spawn(net_task(net_runner).unwrap());
    spawner.spawn(dhcp_server_task(net_stack).unwrap());
    spawner.spawn(dns_hijack_task(net_stack).unwrap());
    spawner.spawn(portal::web_task(net_stack, stored_ssid, stored_url, password_is_set).unwrap());

    // Hand the panel rendering off to its own task — composing the QR
    // + instructions bitmap and driving the ~20 s e-ink refresh all run
    // in the background so a fast Save / Refresh press isn't blocked
    // waiting for them. The software reset at the end of this function
    // cleanly interrupts the driver mid-update if the user wins the
    // race; the next boot's own panel reset recovers.
    spawner.spawn(panel_render_task(spi_bus, epd, ap_ssid).unwrap());

    // Two exits from config mode:
    //   - the HTTP portal fires `SAVE_SIGNAL` with the submitted form →
    //     persist to NVS, reset.
    //   - the user presses the Refresh button → don't touch NVS, reset
    //     anyway (next boot lands in normal mode with the existing
    //     creds).
    // A failure to write NVS is logged but we reset regardless: either
    // partial success lands new values for the next boot, or NVS is
    // still incomplete and we re-enter config mode for a retry. Either
    // beats silently hanging after the user hit "Save".
    println!("Config mode ready — awaiting save or Refresh press.");
    let mut refresh_input =
        Input::new(gpio_btn_refresh, InputConfig::default().with_pull(Pull::Up));
    let decision = embassy_futures::select::select(
        portal::SAVE_SIGNAL.wait(),
        wait_for_press(&mut refresh_input, Duration::from_millis(50)),
    )
    .await;
    match decision {
        embassy_futures::select::Either::First(creds) => {
            println!(
                "Saving config to NVS: ssid={:?}, url={:?}, password={}",
                creds.ssid,
                creds.url,
                if creds.password.is_some() {
                    "updated"
                } else {
                    "unchanged"
                }
            );
            if let Err(e) = nvs.set_wifi_ssid(&creds.ssid) {
                println!("WARNING: failed to write wifi.ssid: {:?}", e);
            }
            if let Some(pw) = creds.password.as_deref() {
                if let Err(e) = nvs.set_wifi_password(pw) {
                    println!("WARNING: failed to write wifi.pass: {:?}", e);
                }
            }
            if let Err(e) = nvs.set_image_url(&creds.url) {
                println!("WARNING: failed to write image.url: {:?}", e);
            }
            // Any cached BSSID/channel belonged to the *previous*
            // SSID; if creds just changed it's stale, so drop it
            // and let the next boot scan fresh.
            crate::single_shot_wifi::clear_hint();
            // Give the HTTP response a moment to flush before we reboot.
            Timer::after(Duration::from_millis(500)).await;
        }
        embassy_futures::select::Either::Second(()) => {
            println!("Refresh pressed — leaving config mode without saving.");
        }
    }

    println!("Rebooting");
    esp_hal::system::software_reset();
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, esp_radio::wifi::Interface<'static>>) {
    runner.run().await
}

/// Render the QR + instructions frame for configuration mode and drive
/// the e-ink refresh, off the hot path so the save / Refresh race in
/// `run` isn't blocked for ~20 s waiting for the panel to settle.
#[embassy_executor::task]
async fn panel_render_task(
    mut spi_bus: Spi<'static, esp_hal::Async>,
    mut epd: EpdPanel,
    ap_ssid: String,
) {
    let (panel_width, panel_height) = (EpdPanel::WIDTH, EpdPanel::HEIGHT);
    // The QR encodes a WiFi-join URI so most phones' camera-app scanners
    // offer a one-tap "Join network" action; the text below it gives the
    // SSID (for manual entry) and the portal URL as a fallback for users
    // whose phones don't surface the captive-portal popup automatically.
    let qr_payload = format!("WIFI:T:nopass;S:{};;", ap_ssid);
    let instructions = format!(
        "epd-photoframe Setup\n\
         \n\
         Scan the QR code, or join WiFi: {}\n\
         \n\
         Then open: http://192.168.4.1/\n\
         \n\
         Press the {} to exit without saving.",
        ap_ssid,
        portal::REFRESH_BUTTON_LABEL
    );
    println!("Rendering config screen with QR: {}", qr_payload);
    let frame =
        config_image::render::<EpdColor>(panel_width, panel_height, &qr_payload, &instructions);

    // Bring the panel's enable rail up before any panel I/O.
    // Idempotent — fine if the white pre-flash already raised it.
    epd.enable().await.unwrap();
    println!("Reset");
    epd.reset().await.unwrap();
    println!("Wait until idle");
    epd.wait_until_idle().await.unwrap();
    println!("Init");
    // The config-mode frame is rendered with `BLACK` + `WHITE` only
    // (QR plus instruction text); ask the panel which init mode covers
    // that. On E1001 that's `Bw`; single-mode panels return `()`.
    let init_mode = EpdPanel::init_mode_for_palette([EpdColor::BLACK, EpdColor::WHITE]);
    epd.init(&mut spi_bus, init_mode).await.unwrap();
    println!("Power on");
    epd.power_on(&mut spi_bus).await.unwrap();
    println!("Update frame (QR)");
    let data = (0..(panel_width * panel_height)).map(|idx| {
        let (x, y) = EpdPanel::output_index_to_image_xy(idx);
        frame[y * panel_width + x]
    });
    epd.update_frame(&mut spi_bus, data).await.unwrap();
    println!("Trigger refresh");
    epd.display_frame_no_wait(&mut spi_bus).await.unwrap();
    println!("Wait until idle (~20s refresh)");
    epd.wait_until_idle().await.unwrap();
    println!("Power off");
    epd.power_off(&mut spi_bus).await.unwrap();
    epd.disable().await.unwrap();
    println!("Config panel render done");
}

/// Keep the WiFi controller alive and log association events. The
/// controller must outlive the lifetime of the AP, hence the idle loop
/// that holds onto it.
#[embassy_executor::task]
async fn ap_wifi_task(controller: esp_radio::wifi::WifiController<'static>) {
    println!("AP started");
    loop {
        // Log association events as they come in; they're useful for
        // verifying a phone actually joined the portal network.
        match controller
            .wait_for_access_point_connected_event_async()
            .await
        {
            Ok(esp_radio::wifi::AccessPointStationEventInfo::Connected(info)) => {
                println!("AP: station connected: {:?}", info);
            }
            Ok(esp_radio::wifi::AccessPointStationEventInfo::Disconnected(info)) => {
                println!("AP: station disconnected: {:?}", info);
            }
            Err(e) => {
                println!("AP: event-wait error: {:?}", e);
            }
        }
    }
}

/// Serve DHCP on the AP interface so phones joining the captive-portal
/// network get an IP address immediately (otherwise modern phones time
/// out the join and drop the network as "no internet"). The server runs
/// on top of `edge-nal-embassy`'s UDP socket and uses `edge-dhcp`'s
/// state machine — same abstraction stack as the DNS hijack and HTTP
/// portal, so all three services share buffers/idioms.
#[embassy_executor::task]
async fn dhcp_server_task(stack: Stack<'static>) {
    use core::net::{IpAddr, Ipv4Addr, SocketAddr};

    use edge_nal::UdpBind;

    let ap_ip = Ipv4Addr::new(192, 168, 4, 1);
    let udp_buffers: edge_nal_embassy::UdpBuffers<1, 1500, 1500, 2> =
        edge_nal_embassy::UdpBuffers::new();
    let udp = edge_nal_embassy::Udp::new(stack, &udp_buffers);

    let dns_servers = [ap_ip];
    let mut gateway_buf = [Ipv4Addr::UNSPECIFIED; 1];
    let mut server_options = edge_dhcp::server::ServerOptions::new(ap_ip, Some(&mut gateway_buf));
    server_options.dns = &dns_servers;

    // `edge-dhcp` defaults the pool to .50-.200 of the server's /24;
    // keep that instead of pinning it here. MAX_CLIENTS=8 is generous
    // for a captive-portal network that usually sees one phone.
    let mut server =
        edge_dhcp::server::Server::<_, 8>::new(|| embassy_time::Instant::now().as_secs(), ap_ip);

    let mut buf = [0u8; 1500];
    loop {
        match udp
            .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 67))
            .await
        {
            Ok(mut socket) => {
                match edge_dhcp::io::server::run(
                    &mut server,
                    &server_options,
                    &mut socket,
                    &mut buf,
                )
                .await
                {
                    Ok(()) => println!("DHCP server returned Ok; restarting"),
                    Err(e) => println!("DHCP server error ({:?}); restarting", e),
                }
            }
            Err(e) => println!("DHCP bind :67 failed ({:?}); retrying", e),
        }
        Timer::after(Duration::from_millis(500)).await;
    }
}

/// Captive-portal DNS hijack: reply to every `A` query with the AP's own
/// IP address. That's what makes iOS/Android/Windows pop the "sign in to
/// this network" prompt automatically after they join.
#[embassy_executor::task]
async fn dns_hijack_task(stack: Stack<'static>) {
    use core::net::{IpAddr, Ipv4Addr, SocketAddr};

    let ap_ip = Ipv4Addr::new(192, 168, 4, 1);
    let buffers: edge_nal_embassy::UdpBuffers<1, 512, 512, 2> = edge_nal_embassy::UdpBuffers::new();
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
