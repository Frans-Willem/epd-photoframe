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

use alloc::string::String;
use alloc::vec::Vec;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_println::println;

use crate::config::Config;
use crate::net_resources::NETWORK_RESOURCES;

/// Cached BSSID + channel of the AP we last associated with. Pinning
/// both at the next association lets the radio go straight to that
/// channel and AP without scanning, shaving ~1–2 s off the connect
/// phase. Persisted as an opaque byte blob by the [`WifiCredStore`]
/// alongside the credentials it was learned with: when those
/// credentials are rewritten the store also drops the hint, so a
/// stale one can't survive a network change. If the pinned attempt
/// fails anyway (AP roamed channel or BSSID), `wifi_runner` retries
/// once with a no-hint fallback config in the same session, so the
/// worst case is one extra connect-attempt's worth of time.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct WifiHint {
    bssid: [u8; 6],
    channel: u8,
}

/// Wire format for the persisted hint: 6 BSSID bytes followed by 1
/// channel byte. The store sees only `Vec<u8>` so [`Config`] doesn't
/// need to know what's inside — corruption recovery (wrong-length
/// blob) lives on the decode side via `TryFrom`.
impl From<WifiHint> for Vec<u8> {
    fn from(h: WifiHint) -> Vec<u8> {
        let mut v = Vec::with_capacity(7);
        v.extend_from_slice(&h.bssid);
        v.push(h.channel);
        v
    }
}

impl TryFrom<Vec<u8>> for WifiHint {
    type Error = ();
    fn try_from(b: Vec<u8>) -> Result<Self, ()> {
        if b.len() != 7 {
            return Err(());
        }
        let mut bssid = [0u8; 6];
        bssid.copy_from_slice(&b[..6]);
        Ok(WifiHint {
            bssid,
            channel: b[6],
        })
    }
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
    /// The credential store returned an error reading SSID / password
    /// (flash trouble, malformed entry).
    CredentialsError,
    /// The credential store returned `None` for a required field
    /// (SSID or password). `is_configured` should normally rule this
    /// out before `run` is called.
    EmptyCredentials,
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
            Error::CredentialsError => format!("WiFi credentials read failed"),
            Error::EmptyCredentials => format!("WiFi credentials missing"),
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

/// Abstraction over "the place WiFi credentials + the BSSID/channel
/// hint live". Implemented by [`Config`] (the NVS-backed store) for
/// the production path; the indirection keeps `single_shot_wifi`
/// independent of the storage details and makes the connect path
/// testable with a mock. The `WifiHint` ↔ byte conversion lives in
/// the impl so the store sees only opaque bytes.
pub trait WifiCredStore {
    fn get_ssid(&self) -> Result<String, Error>;
    fn get_password(&self) -> Result<String, Error>;
    fn get_hint(&self) -> Result<Option<WifiHint>, Error>;
    fn set_hint(&mut self, hint: WifiHint) -> Result<(), Error>;
}

impl<'a> WifiCredStore for Config<'a> {
    fn get_ssid(&self) -> Result<String, Error> {
        self.get_wifi_ssid()
            .map_err(|_| Error::CredentialsError)?
            .ok_or(Error::EmptyCredentials)
    }
    fn get_password(&self) -> Result<String, Error> {
        self.get_wifi_password()
            .map_err(|_| Error::CredentialsError)?
            .ok_or(Error::EmptyCredentials)
    }
    fn get_hint(&self) -> Result<Option<WifiHint>, Error> {
        // A wrong-length blob (corrupt entry, schema change) is
        // treated as "no hint" rather than a hard error: the next
        // successful connect overwrites it.
        Ok(self
            .get_wifi_hint()
            .map_err(|_| Error::CredentialsError)?
            .and_then(|bytes| WifiHint::try_from(bytes).ok()))
    }
    fn set_hint(&mut self, hint: WifiHint) -> Result<(), Error> {
        // esp-nvs short-circuits when the stored bytes already match,
        // so calling this every successful connect is fine.
        self.set_wifi_hint(&Vec::from(hint))
            .map_err(|_| Error::CredentialsError)
    }
}

/// Bring up WiFi + embassy-net for one fetch cycle and run `f` with
/// the live `Stack`. See the module docs for the lifecycle / timeout
/// contract.
pub async fn run<'d, T, Fut, F, S>(
    wifi: esp_hal::peripherals::WIFI<'d>,
    creds: &mut S,
    connect_timeout: Duration,
    f: F,
) -> Result<T, Error>
where
    F: FnOnce(embassy_net::Stack<'static>) -> Fut,
    Fut: core::future::Future<Output = T>,
    S: WifiCredStore + ?Sized,
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
    // If the credential store has a cached BSSID + channel from a
    // prior successful association, pin them both for the *initial*
    // attempt — the radio skips the scan and goes straight to that AP.
    //
    // Build a no-hint fallback config in parallel: if the hint-pinned
    // attempt fails (AP changed channel/BSSID, or moved entirely), the
    // runner reconfigures the controller with it and retries once, so
    // a stale hint costs an extra connect-attempt's worth of time
    // rather than a whole error-frame cycle. A fresh hint is written
    // back to the store after every successful connect.
    let hint = creds.get_hint().ok().flatten();
    let base_sta_cfg = esp_radio::wifi::sta::StationConfig::default()
        .with_ssid(creds.get_ssid()?.as_str())
        .with_password(creds.get_password()?.as_str().into());
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
    // reconnecting silently. Only `wifi_runner` borrows `creds`
    // (to write the fresh hint after each successful connect), so
    // the `&mut creds` doesn't conflict with `run_with_wifi`.
    match select(
        run_with_wifi(&status, interfaces, timeout, f),
        wifi_runner(creds, &status, controller, fallback_config),
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
async fn wifi_runner<'d, S: WifiCredStore + ?Sized>(
    creds: &mut S,
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
                        // We're already associated; a hint-write
                        // failure is not worth dropping the connection
                        // for — the worst case is the next boot does
                        // a no-hint scan.
                        if let Err(e) = creds.set_hint(WifiHint {
                            bssid: info.bssid,
                            channel: info.channel,
                        }) {
                            println!("set_hint failed; not caching hint: {:?}", e);
                        }
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
