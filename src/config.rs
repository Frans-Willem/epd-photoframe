//! Persistent runtime configuration, read from and written to the ESP-IDF
//! NVS partition via [`esp_nvs`].
//!
//! Layout: values live under the `"config"` namespace, one entry per field,
//! with keys chosen to stay under the 15-byte NVS limit and to leave room
//! for static-IP fields later (see `PLAN.md`).

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use esp_nvs::{Key, error::Error};

/// ESP-IDF default NVS partition offset (see the project's partition table:
/// `nvs` at `0x9000`, size `0x6000`).
const NVS_OFFSET: usize = 0x9000;
const NVS_SIZE: usize = 0x6000;

const NS: Key = Key::from_str("config");
const K_WIFI_SSID: Key = Key::from_str("wifi.ssid");
const K_WIFI_PASS: Key = Key::from_str("wifi.pass");
const K_IMAGE_URL: Key = Key::from_str("image.url");
/// Last-known BSSID + channel, stored as a 7-byte blob (6 bytes BSSID
/// followed by 1 byte channel). Read on the next boot to pin the radio
/// to that AP and skip the scan; cleared automatically whenever the
/// SSID or password is rewritten so a credential change can't leave a
/// stale hint behind.
const K_WIFI_HINT: Key = Key::from_str("wifi.hint");

/// Owns the flash peripheral and the parsed NVS state for the lifetime of
/// the caller. Construct once at boot; re-use for reads and writes.
///
/// `RefCell` wraps the underlying [`esp_nvs::Nvs`] so callers can use
/// `&self` getters/setters. Single-executor cooperative scheduling means
/// there's no parallel access; borrows never span an await, so a
/// `RefCell` panic is unreachable in practice.
pub struct Config<'d> {
    nvs: RefCell<esp_nvs::Nvs<esp_storage::FlashStorage<'d>>>,
}

impl<'d> Config<'d> {
    /// Open the NVS partition. Returns an error if the partition offset /
    /// size are misconfigured or the stored sectors are unreadable; a fresh
    /// (all-`0xFF`) partition is treated as "no entries yet" rather than an
    /// error.
    pub fn new(flash: esp_hal::peripherals::FLASH<'d>) -> Result<Self, Error> {
        let storage = esp_storage::FlashStorage::new(flash);
        let nvs = esp_nvs::Nvs::new(NVS_OFFSET, NVS_SIZE, storage)?;
        Ok(Self {
            nvs: RefCell::new(nvs),
        })
    }

    /// Read `key` as a `String`; returns `Ok(None)` for the common
    /// "namespace missing" / "key missing" cases (benign, happens on a
    /// device that's never been configured) and `Err` for real flash or
    /// corruption problems.
    fn get_string(&self, key: &Key) -> Result<Option<String>, Error> {
        match self.nvs.borrow_mut().get(&NS, key) {
            Ok(v) => Ok(Some(v)),
            Err(Error::NamespaceNotFound | Error::KeyNotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn is_configured(&self) -> Result<bool, Error> {
        Ok(self.get_wifi_ssid()?.is_some()
            && self.get_wifi_password()?.is_some()
            && self.get_image_url()?.is_some())
    }

    pub fn get_wifi_ssid(&self) -> Result<Option<String>, Error> {
        self.get_string(&K_WIFI_SSID)
    }

    pub fn get_wifi_password(&self) -> Result<Option<String>, Error> {
        self.get_string(&K_WIFI_PASS)
    }

    pub fn get_image_url(&self) -> Result<Option<String>, Error> {
        self.get_string(&K_IMAGE_URL)
    }

    pub fn set_wifi_ssid(&mut self, v: &str) -> Result<(), Error> {
        self.nvs.borrow_mut().set(&NS, &K_WIFI_SSID, v)?;
        // The hint is tied to the credentials it was learned with;
        // rewriting either of them invalidates it.
        self.clear_wifi_hint()
    }

    pub fn set_wifi_password(&mut self, v: &str) -> Result<(), Error> {
        self.nvs.borrow_mut().set(&NS, &K_WIFI_PASS, v)?;
        self.clear_wifi_hint()
    }

    pub fn set_image_url(&mut self, v: &str) -> Result<(), Error> {
        self.nvs.borrow_mut().set(&NS, &K_IMAGE_URL, v)
    }

    /// Read the cached connect hint as an opaque blob. Returns
    /// `Ok(None)` when the slot is missing (fresh device or just
    /// cleared after a credential rewrite). Decoding back into the
    /// hint's logical fields lives at the WiFi layer.
    pub fn get_wifi_hint(&self) -> Result<Option<Vec<u8>>, Error> {
        match self.nvs.borrow_mut().get::<Vec<u8>>(&NS, &K_WIFI_HINT) {
            Ok(v) => Ok(Some(v)),
            Err(Error::NamespaceNotFound | Error::KeyNotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn set_wifi_hint(&mut self, hint: &[u8]) -> Result<(), Error> {
        self.nvs.borrow_mut().set(&NS, &K_WIFI_HINT, hint)
    }

    pub fn clear_wifi_hint(&mut self) -> Result<(), Error> {
        self.nvs.borrow_mut().delete(&NS, &K_WIFI_HINT)
    }
}
