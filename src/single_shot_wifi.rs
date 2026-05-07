//! Single-shot WiFi + embassy-net wrapper.
//!
//! Brings up WiFi as a station, waits for association, waits a
//! short settle delay (`POST_CONNECT_PAUSE`) so the AP's forwarding
//! table has caught up before we start DHCP, brings up the embassy-net
//! stack, waits for DHCP, runs the caller's closure with the live
//! `Stack`, then tears everything down on return.
//!
//! Intended for the classic e-ink wake-fetch-sleep loop: one function
//! call per wake cycle, no long-lived networking state. The `connect`
//! timeout covers the whole setup (connect + optional pause + DHCP);
//! time spent inside the caller's closure is unbounded.
//!
//! On return:
//! - The `WifiController` is dropped (with the `wifi_runner` future),
//!   which per esp-radio 0.18 stops the radio automatically.
//! - The embassy-net `Runner` future is dropped (via the surrounding
//!   `select`), so the stack stops processing packets.
//! - `NETWORK_RESOURCES`/`WIFI<'static>` are single-shot — they can
//!   be given to this function exactly once per boot.

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_println::println;

use crate::hardware::WifiCredentials;
use crate::net_resources::NETWORK_RESOURCES;
use crate::rtc_persisted::RtcPersisted;

/// Cached BSSID + channel of the AP we last associated with. Pinning
/// both at the next association lets the radio go straight to that
/// channel and AP without scanning, shaving ~1–2 s off the connect
/// phase. The slot is `take()`n at use, then re-`set()` on success.
/// If the pinned attempt fails, `wifi_runner` retries once with a
/// no-hint fallback config in the same session, so a stale hint
/// only costs an extra connect-attempt's worth of time.
/// `RtcPersisted`'s magic gate handles the cold-boot "all zeros =
/// no hint" case.
#[esp_hal::ram(unstable(rtc_slow, persistent))]
static WIFI_HINT: RtcPersisted<WifiHint> = RtcPersisted::new();

#[derive(Clone, Copy)]
struct WifiHint {
    bssid: [u8; 6],
    channel: u8,
}

/// Drop any cached BSSID/channel. Call from `config_mode` when the
/// stored WiFi credentials are written: a fresh SSID/password may
/// belong to a different AP, in which case the previous hint would
/// fail and force a fallback scan on every wake until the slot
/// gets overwritten with a fresh hint anyway.
pub fn clear_hint() {
    WIFI_HINT.clear();
}

#[derive(Debug)]
pub enum Error {
    /// `esp_radio::wifi::new` failed — usually a bad `ControllerConfig`
    /// or a radio hardware issue. Upstream error preserved.
    Init(esp_radio::wifi::WifiError),
    /// `connect_async` returned `Err`: wrong SSID/password, SSID
    /// missing, disconnected mid-handshake, etc.
    Connect(esp_radio::wifi::WifiError),
    /// The whole setup phase (connect + pause + DHCP) didn't finish
    /// within the supplied timeout.
    Timeout,
}

impl Error {
    /// Format the error into a user-visible string. `ssid` is the
    /// configured target SSID so the error frame can tell the user
    /// which network failed.
    pub fn message(&self, ssid: &str) -> alloc::string::String {
        use alloc::format;
        match self {
            Error::Init(e) => format!("WiFi init failed: {:?}", e),
            Error::Connect(e) => format!("WiFi connect failed: {:?}\nSSID: {}", e, ssid),
            Error::Timeout => format!("WiFi setup timed out\nSSID: {}", ssid),
        }
    }
}

/// Settle pause between association and bringing up embassy-net.
/// The AP side sometimes drops the first broadcast from a
/// freshly-associated client (DHCP DISCOVER is broadcast); this
/// short pause lets the AP commit the client to its forwarding
/// table first.
const POST_CONNECT_PAUSE: Duration = Duration::from_millis(150);

/// How long to wait before retrying a failed `connect_async`. Only
/// used by `wifi_runner` for silent mid-session reconnects; the
/// initial connect failure is surfaced to the caller directly.
const RECONNECT_RETRY_DELAY: Duration = Duration::from_secs(5);

/// How long to wait after a disconnect event before trying to
/// reconnect. Gives the AP and the radio a moment to settle before
/// we hammer them with another association attempt.
const POST_DISCONNECT_PAUSE: Duration = Duration::from_millis(500);

/// Shared signal between `run_phases` and `wifi_runner` carrying the
/// latest result from `controller.connect_async()`. `wait()`ed once
/// during setup to gate on the initial association; after that the
/// runner keeps reconnecting silently without anyone listening (the
/// signal just overwrites itself each time).
type WifiStatus = Signal<NoopRawMutex, Result<(), esp_radio::wifi::WifiError>>;

/// Bring up WiFi + embassy-net for one fetch cycle and run `f` with
/// the live `Stack`. See the module docs for the lifecycle / timeout
/// contract.
pub async fn run<'d, T, Fut, F>(
    wifi: esp_hal::peripherals::WIFI<'d>,
    creds: &WifiCredentials,
    connect_timeout: Duration,
    f: F,
) -> Result<T, Error>
where
    F: FnOnce(embassy_net::Stack<'static>) -> Fut,
    Fut: core::future::Future<Output = T>,
{
    // Signal carrying the initial association outcome from
    // `wifi_runner` to `run_with_wifi`. Lives on the stack for the
    // lifetime of this call — no cross-call state to worry about.
    let status: WifiStatus = Signal::new();

    // One absolute deadline for the whole connect+DHCP setup phase.
    // Passed by value down the call chain; `run_with_wifi` internally
    // reborrows as `&mut` so it can use the same Timer in phase 1
    // and then hand it on to `run_with_net` for the DHCP wait.
    let timeout = Timer::after(connect_timeout);

    // --- Construct the controller. `ControllerConfig::initial_config`
    // starts the radio in station mode with the supplied credentials. ---
    //
    // If we have a cached BSSID + channel from a prior successful
    // association, pin them both for the *initial* attempt — the
    // radio skips the scan and goes straight to that AP. Take()n out
    // of the slot so a failed association doesn't lock us into
    // retrying with a stale hint; a fresh one is written back after
    // a successful connect.
    //
    // Build a no-hint fallback config in parallel: if the hint-pinned
    // attempt fails (AP changed channel/BSSID, or moved entirely), the
    // runner reconfigures the controller with this and retries once,
    // so a stale hint costs an extra connect-attempt's worth of time
    // rather than a whole error-frame cycle.
    let hint = WIFI_HINT.take();
    let base_sta_cfg = esp_radio::wifi::sta::StationConfig::default()
        .with_ssid(creds.ssid.as_str())
        .with_password(creds.password.as_str().into());
    let (initial_sta_cfg, fallback_config) = match hint {
        Some(h) => {
            println!(
                "WiFi hint: BSSID {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, channel {}",
                h.bssid[0], h.bssid[1], h.bssid[2], h.bssid[3], h.bssid[4], h.bssid[5], h.channel
            );
            let pinned = base_sta_cfg
                .clone()
                .with_bssid(h.bssid)
                .with_channel(h.channel);
            (
                pinned,
                Some(esp_radio::wifi::Config::Station(base_sta_cfg)),
            )
        }
        None => {
            println!("WiFi hint: none (cold boot or stale; will scan)");
            (base_sta_cfg, None)
        }
    };
    let initial_config = esp_radio::wifi::Config::Station(initial_sta_cfg);
    let controller_config =
        esp_radio::wifi::ControllerConfig::default().with_initial_config(initial_config);
    let (controller, interfaces) =
        esp_radio::wifi::new(wifi, controller_config).map_err(Error::Init)?;

    println!("WiFi connecting (timeout {} s)", connect_timeout.as_secs());

    // Phases 1 (associate) through 3 (f) all run concurrently with
    // `wifi_runner`, which handles mid-session disconnects by
    // reconnecting silently.
    match select(
        run_with_wifi(&status, interfaces, timeout, f),
        wifi_runner(&status, controller, fallback_config),
    )
    .await
    {
        Either::First(result) => result,
        Either::Second(_) => unreachable!("wifi_runner never returns"),
    }
    // `controller` dropped (with `wifi_runner`): radio stops.
    // `interfaces` / `stack` / `net_runner` all drop too.
}

/// The body that runs *alongside* `wifi_runner`: waits for the initial
/// association, holds the settle pause, then hands off to
/// `run_with_net` for everything that needs the net stack.
async fn run_with_wifi<'d, T, Fut, F>(
    status: &WifiStatus,
    interfaces: esp_radio::wifi::Interfaces<'d>,
    mut timeout: Timer,
    f: F,
) -> Result<T, Error>
where
    F: FnOnce(embassy_net::Stack<'static>) -> Fut,
    Fut: core::future::Future<Output = T>,
{
    // --- Phase 1: wait for `wifi_runner` to report the initial
    // association, bounded by the shared timeout. `&mut timeout`
    // re-borrows here so we can keep the Timer for phase 3. ---
    match select(status.wait(), &mut timeout).await {
        Either::First(Ok(())) => {
            println!("WiFi connected after {} ms", Instant::now().as_millis());
        }
        Either::First(Err(e)) => return Err(Error::Connect(e)),
        Either::Second(()) => return Err(Error::Timeout),
    }

    // --- Phase 2: settle pause; see `POST_CONNECT_PAUSE` doc. ---
    Timer::after(POST_CONNECT_PAUSE).await;

    // --- Phase 3: bring up embassy-net on the STA interface with
    // DHCP. The runner (`Runner::run`) must be polled concurrently
    // with everything from here on — it's what actually processes
    // packets, drives smoltcp's DHCP client, and so on. ---
    let rng = esp_hal::rng::Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let (stack, mut net_runner) = embassy_net::new(
        interfaces.station,
        embassy_net::Config::dhcpv4(Default::default()),
        NETWORK_RESOURCES.take(),
        seed,
    );

    match select(run_with_net(stack, timeout, f), net_runner.run()).await {
        Either::First(result) => result,
        Either::Second(_) => unreachable!("net_runner.run() returns !"),
    }
}

/// The body that runs *alongside* the embassy-net `Runner`: waits for
/// DHCP to come up (bounded by the shared `timeout`), then hands
/// the live `Stack` to the caller's closure. Time spent inside `f`
/// is unbounded — the timeout only gates the DHCP wait.
async fn run_with_net<T, Fut, F>(
    stack: embassy_net::Stack<'static>,
    timeout: Timer,
    f: F,
) -> Result<T, Error>
where
    F: FnOnce(embassy_net::Stack<'static>) -> Fut,
    Fut: core::future::Future<Output = T>,
{
    match select(stack.wait_config_up(), timeout).await {
        Either::First(()) => {
            println!("DHCP config up after {} ms", Instant::now().as_millis());
            Ok(f(stack).await)
        }
        Either::Second(()) => Err(Error::Timeout),
    }
}

/// Own the `WifiController` for the lifetime of the outer `select` and
/// keep the radio connected. Signals `status` once on each (re)connect
/// so the main work can gate on the *initial* association; mid-session
/// disconnects trigger a silent reconnect.
///
/// `fallback_config` carries the no-hint config when the initial
/// attempt was made with a cached BSSID/channel hint: on the first
/// failure the runner reconfigures the controller with it and retries
/// once before surfacing the error. `None` means "the initial config
/// is the only one to try" — failures go straight to `status.signal`.
/// The slot is `take()`n on each loop iteration so the fallback fires
/// at most once per session even if a successful `Ok` re-arms the
/// reconnect path.
async fn wifi_runner<'d>(
    status: &WifiStatus,
    mut controller: esp_radio::wifi::WifiController<'d>,
    mut fallback_config: Option<esp_radio::wifi::Config>,
) -> ! {
    loop {
        match (controller.connect_async().await, fallback_config.take()) {
            (Ok(_), _) => {
                // Cache the AP we just associated with so the next
                // boot can pin BSSID + channel and skip the scan.
                // Failure to read ap_info is non-fatal — we just
                // skip caching for this cycle.
                match controller.ap_info() {
                    Ok(info) => {
                        println!(
                            "Connected to BSSID {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} on channel {}",
                            info.bssid[0], info.bssid[1], info.bssid[2],
                            info.bssid[3], info.bssid[4], info.bssid[5],
                            info.channel
                        );
                        WIFI_HINT.set(WifiHint {
                            bssid: info.bssid,
                            channel: info.channel,
                        });
                    }
                    Err(e) => println!("ap_info() failed; not caching hint: {:?}", e),
                }
                status.signal(Ok(()));
                // Block until the station drops off the AP, then
                // pause briefly before reconnecting. The `Ok` we
                // just signalled may or may not have been consumed
                // by the main task; the next signal overwrites it
                // regardless.
                let info = controller.wait_for_disconnect_async().await.ok();
                println!(
                    "WiFi disconnected: {:?}, reconnecting in {} ms",
                    info,
                    POST_DISCONNECT_PAUSE.as_millis()
                );
                Timer::after(POST_DISCONNECT_PAUSE).await;
            }
            (Err(e), Some(fb)) => {
                // Stale hint? Drop the BSSID/channel pin and retry
                // immediately with the no-hint config — that scan
                // will find the AP wherever it actually lives now.
                // If the no-hint attempt also fails on the next
                // iteration, `fallback_config` is already `None`,
                // so we'll match the (Err, None) arm and surface
                // the error normally.
                println!(
                    "WiFi connect with cached hint failed: {:?}; falling back to scan",
                    e
                );
                if let Err(ce) = controller.set_config(&fb) {
                    println!("set_config to fallback failed: {:?}", ce);
                    status.signal(Err(ce));
                    Timer::after(RECONNECT_RETRY_DELAY).await;
                }
            }
            (Err(e), None) => {
                println!(
                    "WiFi connect failed: {:?}, retry in {} s",
                    e,
                    RECONNECT_RETRY_DELAY.as_secs()
                );
                status.signal(Err(e));
                Timer::after(RECONNECT_RETRY_DELAY).await;
            }
        }
    }
}
