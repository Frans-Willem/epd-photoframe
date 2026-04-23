//! Reusable slot of persistent storage living in RTC-slow RAM:
//! survives deep sleep, zeroed on a cold power-on. Callers place an
//! `RtcPersisted<T>` in a `#[esp_hal::ram(unstable(rtc_slow,
//! persistent))]` static and use its `take` / `set` / `clear`
//! methods.
//!
//! Integrity contract for each slot:
//!
//! 1. **Magic marker** written *after* the payload + checksum. A reset
//!    that interrupts `set()` leaves `magic == 0` (cold-boot state),
//!    and the next `take()` sees "nothing stored."
//! 2. **CRC-32 checksum** over the payload bytes. Catches the case
//!    where the magic slipped through but the payload was torn, plus
//!    any bit-rot during the RTC-SLOW domain's lower-power retention.
//!
//! `RtcPersisted<T>` exposes `take`/`set`/`clear` through `&self` via
//! interior mutability — callers reference the static directly, no
//! `static mut` raw-pointer dance.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;

/// Fixed sanity marker written next to the payload so a read after a
/// cold boot (when all RTC-persistent data is zero) can't be mistaken
/// for real content. Any non-zero constant works; this one is just
/// recognisable in a hex dump.
const MAGIC: u32 = 0xE100_2137;

#[repr(C)]
struct Inner<T> {
    magic: u32,
    checksum: u32,
    payload: MaybeUninit<T>,
}

/// Generic slot of persistent RTC memory. `T` is stored inside a
/// `MaybeUninit` so its validity invariants don't have to survive the
/// cold-boot all-zeros state — the `magic` + checksum dance is what
/// gates `assume_init_read` on the way out. The `UnsafeCell` wrapper
/// means access goes through `&self` rather than `&mut self`, letting
/// the static slots be plain `static`s.
#[repr(transparent)]
pub struct RtcPersisted<T> {
    inner: UnsafeCell<Inner<T>>,
}

// SAFETY: this crate only ever touches `RtcPersisted` slots from the
// single `main()` task — no interrupt handler, no concurrent executor
// task. Placing the slot in a `static` therefore doesn't risk aliased
// mutable access, and `take`/`set`/`clear` never overlap their own
// `&mut Inner<T>` borrows. The `Sync` impl documents that invariant.
unsafe impl<T> Sync for RtcPersisted<T> {}

// SAFETY: `RtcPersisted<T>` is `#[repr(transparent)]` over
// `UnsafeCell<Inner<T>>`, and `UnsafeCell` is also `repr(transparent)`,
// so the layout in RTC memory is exactly `Inner<T>`. `Inner<T>` has
// only `u32` primitives and a `MaybeUninit<T>` (no validity
// invariants), so any bit pattern in RTC memory is valid — which is
// what `Persistable` requires.
unsafe impl<T> esp_hal::Persistable for RtcPersisted<T> {}

impl<T> RtcPersisted<T> {
    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(Inner {
                magic: 0,
                checksum: 0,
                payload: MaybeUninit::uninit(),
            }),
        }
    }

    /// Return a clone of the stored value, leaving the slot intact
    /// so subsequent calls still see it. Same validation as `take`:
    /// returns `None` on cold boot, a torn write (no magic), or a
    /// checksum mismatch.
    pub fn get(&self) -> Option<T>
    where
        T: Clone,
    {
        // SAFETY: single-threaded access (see `Sync` impl note).
        let inner = unsafe { &*self.inner.get() };
        if inner.magic != MAGIC {
            return None;
        }
        if checksum(&inner.payload) != inner.checksum {
            return None;
        }
        // SAFETY: magic + CRC match → `payload` holds a valid `T`.
        Some(unsafe { inner.payload.assume_init_ref() }.clone())
    }

    /// Consume whatever was stored before the last deep sleep. Returns
    /// `None` on a cold boot, after a previous take, if the write was
    /// interrupted (no magic), or if the checksum doesn't match (bit
    /// rot / corruption). Always clears the slot before returning, so
    /// the next `take()` without an intervening `set()` sees "nothing
    /// stored."
    pub fn take(&self) -> Option<T> {
        // SAFETY: single-threaded access (see `Sync` impl note).
        let inner = unsafe { &mut *self.inner.get() };
        let magic = inner.magic;
        let stored_checksum = inner.checksum;
        // Clear eagerly so a mismatch (or a bogus magic from a corrupt
        // frame) isn't sticky.
        inner.magic = 0;
        inner.checksum = 0;
        if magic != MAGIC {
            return None;
        }
        if checksum(&inner.payload) != stored_checksum {
            return None;
        }
        // SAFETY: `set()` wrote a valid `T` into `payload` before
        // stamping the magic, and the CRC over those exact bytes just
        // matched. The bit pattern we're about to assume is therefore
        // a valid `T` that survived the deep sleep intact.
        Some(unsafe { inner.payload.assume_init_read() })
    }

    /// Store `value` for the next wake. Payload is written first, then
    /// the checksum, then the magic — a reset interrupting this call
    /// can at worst leave `magic == 0`, which `take()` treats as
    /// "nothing stored."
    pub fn set(&self, value: T) {
        // SAFETY: single-threaded access (see `Sync` impl note).
        let inner = unsafe { &mut *self.inner.get() };
        inner.payload = MaybeUninit::new(value);
        inner.checksum = checksum(&inner.payload);
        inner.magic = MAGIC;
    }

    pub fn clear(&self) {
        // SAFETY: single-threaded access (see `Sync` impl note).
        let inner = unsafe { &mut *self.inner.get() };
        inner.magic = 0;
        inner.checksum = 0;
    }
}

impl<T> Default for RtcPersisted<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Checksum of the raw bytes of a `MaybeUninit<T>`. Safe to read as
/// `u8` regardless of init state — all bit patterns are valid `u8`s.
fn checksum<T>(p: &MaybeUninit<T>) -> u32 {
    // SAFETY: `p.as_ptr()` is a valid pointer into owned storage of
    // length `size_of::<T>()`; reading as `u8` is always valid.
    let bytes =
        unsafe { core::slice::from_raw_parts(p.as_ptr().cast::<u8>(), core::mem::size_of::<T>()) };
    esp_hal::rom::crc::crc32_le(0, bytes)
}

