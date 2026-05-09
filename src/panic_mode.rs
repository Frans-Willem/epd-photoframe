//! Persistent-panic capture and on-panel rendering.
//!
//! Owns the bits of the firmware that exist to make a panicked
//! boot visible to the user instead of leaving the panel frozen on
//! whatever it last drew:
//!
//! - [`PANIC_MSG`] — an RTC-slow `RtcPersisted` slot for the formatted
//!   `PanicInfo`, so the message survives the soft-reset that the
//!   handler triggers.
//! - [`REBOOT_ALLOWED`] — boot-loop guard. The handler only stashes +
//!   soft-resets when this is `true`; otherwise it halts for JTAG.
//!   `main()` flips it on once it's confirmed the current boot is not
//!   already rendering a previous panic.
//! - The `#[panic_handler]` itself, replacing `esp_backtrace`'s
//!   default. Always prints the panic + a backtrace (using
//!   `esp_backtrace::Backtrace::capture`) to UART; on debug builds /
//!   guard-still-false it halts, on release builds with the guard set
//!   it captures + soft-resets.
//! - [`run`] — analogous to [`crate::config_mode::run`]: takes the
//!   `HardwareCtx`, draws the captured message via
//!   [`crate::error_image::render`], and deep-sleeps until the timer
//!   or a button wakes the device.

use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use esp_println::println;

use crate::error_image;
use crate::hardware::HardwareCtx;
use crate::panel::{Panel, PanelColor};
use crate::rtc_persisted::RtcPersisted;
use crate::uart::wait_for_tx_idle;

/// Captured panic message from a previous boot. The release-build
/// `#[panic_handler]` formats `PanicInfo` into this slot before
/// soft-resetting; on the next boot, `main()` calls
/// [`take_pending_message`] and dispatches into [`run`] when something
/// is present. 256 bytes is enough for the typical
/// "panicked at file:line: msg" footprint — longer messages get
/// truncated, which is fine.
const PANIC_MSG_MAX: usize = 256;
#[esp_hal::ram(unstable(rtc_slow, persistent))]
static PANIC_MSG: RtcPersisted<heapless::String<PANIC_MSG_MAX>> = RtcPersisted::new();

/// Boot-loop guard for the panic-to-screen path. Stays `false` through
/// early boot and the panic-render branch; flipped to `true` by
/// [`allow_reboot`] only once `main()` knows this cycle is *not*
/// rendering a previous panic. The panic handler consults it: if still
/// `false`, halt for JTAG regardless of build profile, mimicking
/// `esp_backtrace`'s default. That way a deterministic re-panic during
/// the panic-render path can't keep stashing + soft-resetting forever
/// — the first re-panic halts, the user still sees the previous panic
/// message on the panel, and JTAG can attach to investigate.
static REBOOT_ALLOWED: AtomicBool = AtomicBool::new(false);

/// Allow the panic handler to soft-reset on the next panic. Called by
/// `main()` once it has confirmed the current cycle is not rendering a
/// previous panic.
pub fn allow_reboot() {
    REBOOT_ALLOWED.store(true, Ordering::Relaxed);
}

/// Return any captured panic message from a previous boot, clearing
/// the slot. `None` on a cold boot or when the previous boot didn't
/// stash anything.
pub fn take_pending_message() -> Option<heapless::String<PANIC_MSG_MAX>> {
    PANIC_MSG.take()
}

/// Replacement for `esp_backtrace`'s default panic handler. Always
/// prints the panic info + a backtrace to UART (matching what
/// `esp_backtrace` would print, using its public `Backtrace::capture`
/// API), then dispatches on the saved [`REBOOT_ALLOWED`] value and
/// `cfg(debug_assertions)`:
///
/// - Guard was `false` at entry (early boot, inside the panic-render
///   path, or a recursive panic — see below): halt unconditionally,
///   mimicking `esp_backtrace`'s default. That way a deterministic
///   re-panic during render can't loop, and JTAG can attach to
///   whichever panic actually broke things.
/// - Guard was `true`, debug build: halt for JTAG inspection. The
///   redundant branch is intentional; `cfg(debug_assertions)` keeps
///   debug builds JTAG-friendly even for "normal" panics that fired
///   well after boot.
/// - Guard was `true`, release build: format the panic info into
///   [`PANIC_MSG`] and soft-reset. The next boot picks up the slot,
///   renders the message on the panel via [`run`], and goes back to
///   deep sleep — so users in the field see *what* crashed instead of
///   a frozen panel.
///
/// **Recursive-panic safety.** The very first thing this handler does
/// is atomically `swap(false)` on `REBOOT_ALLOWED` and stash the prior
/// value in a local — the global flag is now `false` for the rest of
/// the handler's lifetime. If anything later in the body itself panics
/// (`println!` / `Backtrace::capture` / `write!` into the heapless
/// String / `PANIC_MSG.set`), the recursive `#[panic_handler]` call
/// sees the cleared global and falls into the halt branch instead of
/// re-stashing and looping a soft-reset.
///
/// `Backtrace::capture` and `software_reset` are the only allocator-
/// /executor-free primitives we touch here; `heapless::String` +
/// `core::fmt::Write` keeps the message capture allocation-free.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Atomic test-and-clear of the boot-loop guard. The global is now
    // `false` for any nested panic that fires inside this handler;
    // `reboot_allowed` retains the value we entered with and drives
    // the dispatch below.
    let reboot_allowed = REBOOT_ALLOWED.swap(false, Ordering::Relaxed);

    println!();
    println!("====================== PANIC ======================");
    println!("{}", info);
    println!();
    println!("Backtrace:");
    let backtrace = esp_backtrace::Backtrace::capture();
    for frame in backtrace.frames() {
        println!("0x{:x}", frame.program_counter());
    }

    if !reboot_allowed || cfg!(debug_assertions) {
        // Halt for JTAG. Reached either before `main()` set the
        // boot-loop guard, on a debug build, or on a recursive panic
        // (the swap above cleared the global before this re-entry).
        // Force the status LED on first — the embassy executor stops
        // here (we never `.await` again) so the regular `blink_task`
        // won't be polling, and an LED frozen mid-toggle could read
        // as "off" and look indistinguishable from a power-cycled
        // device.
        force_led_on();
        println!("(halting; PANIC_REBOOT_ALLOWED={})", reboot_allowed);
        loop {
            core::hint::spin_loop();
        }
    }

    // Release + guard was true: stash the formatted panic info, drain
    // UART, and soft-reset. RTC-slow RAM survives software reset, so
    // the next boot can render the message.
    let mut msg: heapless::String<PANIC_MSG_MAX> = heapless::String::new();
    let _ = write!(&mut msg, "{}", info);
    PANIC_MSG.set(msg);
    wait_for_tx_idle();
    esp_hal::system::software_reset();
}

/// Placeholder for the panic handler's halt branch. The selected binary owns
/// the status LED pin, so the shared panic path no longer pokes a device
/// specific GPIO register directly.
fn force_led_on() {
    // The device-specific LED pin now lives in the selected binary. Keep the
    // panic path allocation/executor-free and skip forcing a board LED here.
}

/// Render the captured panic `message` on the panel as an error frame
/// and deep-sleep until a button is pressed (or the user power-cycles
/// the device). Mirrors [`crate::config_mode::run`]'s shape: takes
/// ownership of the [`HardwareCtx`] and never returns. Intended to be
/// called only when [`take_pending_message`] returned `Some` for the
/// current boot; the slot has already been cleared by that take, so a
/// re-panic during render falls back to a normal cycle on the next
/// boot rather than looping.
///
/// Deliberately does *not* schedule a timer wake: a panic is an
/// asserted-state failure, not a transient one, so retrying on a
/// fixed interval would just re-render the same message every time
/// (and the panel can't update from deep sleep without a wake). The
/// rendered frame also omits the "Will retry in …" line that
/// transient-error frames carry, since there's nothing scheduled to
/// retry.
pub async fn run<P>(ctx: HardwareCtx<P>, message: &str) -> !
where
    P: Panel<esp_hal::spi::master::Spi<'static, esp_hal::Async>>,
    P::Color: PanelColor,
    P::Error: core::fmt::Debug,
    P::InitMode: core::fmt::Debug,
{
    let HardwareCtx {
        rtc,
        mut gpio_btn_refresh,
        mut gpio_btn_previous,
        mut gpio_btn_next,
        mut spi_bus,
        mut epd,
        ..
    } = ctx;

    println!("PANIC_MSG present; rendering panic frame");
    println!("Panic: {}", message);

    let panel_width = P::WIDTH;
    let panel_height = P::HEIGHT;
    let frame: Vec<P::Color> = error_image::render(panel_width, panel_height, message, None);
    let init_mode = P::init_mode_for_palette([P::Color::BLACK, P::Color::WHITE]);

    // Bring the panel's enable rail up before any panel I/O.
    epd.enable().await.unwrap();
    println!("Reset");
    epd.reset().await.unwrap();
    println!("Wait until idle");
    epd.wait_until_idle().await.unwrap();
    println!("Init");
    epd.init(&mut spi_bus, init_mode).await.unwrap();
    println!("Power on");
    epd.power_on(&mut spi_bus).await.unwrap();
    println!("Update frame (panic)");
    epd.update_frame(
        &mut spi_bus,
        (0..(panel_width * panel_height)).map(|idx| {
            let (x, y) = P::output_index_to_image_xy(idx);
            frame[y * panel_width + x]
        }),
    )
    .await
    .unwrap();
    println!("Trigger refresh");
    epd.display_frame_no_wait(&mut spi_bus).await.unwrap();
    println!("Wait until idle (~20s refresh)");
    epd.wait_until_idle().await.unwrap();
    println!("Power off");
    epd.power_off(&mut spi_bus).await.unwrap();
    epd.disable().await.unwrap();

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
    println!("Panic render done; deep-sleeping (button-only wake)");
    crate::sleep::start_sleep(rtc, None, wakeup_pins);
}
