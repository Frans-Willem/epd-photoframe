//! Persistent runtime configuration, read from and written to the ESP-IDF
//! NVS partition via [`esp_nvs`].
//!
//! Layout: values live under the `"config"` namespace, one entry per field,
//! with keys chosen to stay under the 15-byte NVS limit and to leave room
//! for static-IP fields later (see `PLAN.md`).

use alloc::string::String;
use esp_nvs::{Key, error::Error};

/// ESP-IDF default NVS partition offset (see the project's partition table:
/// `nvs` at `0x9000`, size `0x6000`).
const NVS_OFFSET: usize = 0x9000;
const NVS_SIZE: usize = 0x6000;

const NS: Key = Key::from_str("config");
const K_WIFI_SSID: Key = Key::from_str("wifi.ssid");
const K_WIFI_PASS: Key = Key::from_str("wifi.pass");
const K_IMAGE_URL: Key = Key::from_str("image.url");

/// Owns the flash peripheral and the parsed NVS state for the lifetime of
/// the caller. Construct once at boot; re-use for reads and writes.
pub struct Config<'d> {
    nvs: esp_nvs::Nvs<esp_storage::FlashStorage<'d>>,
}

impl<'d> Config<'d> {
    /// Open the NVS partition. Returns an error if the partition offset /
    /// size are misconfigured or the stored sectors are unreadable; a fresh
    /// (all-`0xFF`) partition is treated as "no entries yet" rather than an
    /// error.
    pub fn new(flash: esp_hal::peripherals::FLASH<'d>) -> Result<Self, Error> {
        let storage = esp_storage::FlashStorage::new(flash);
        let nvs = esp_nvs::Nvs::new(NVS_OFFSET, NVS_SIZE, storage)?;
        Ok(Self { nvs })
    }

    /// Read `key` as a `String`; returns `Ok(None)` for the common
    /// "namespace missing" / "key missing" cases (benign, happens on a
    /// device that's never been configured) and `Err` for real flash or
    /// corruption problems.
    fn get_string(&mut self, key: &Key) -> Result<Option<String>, Error> {
        match self.nvs.get(&NS, key) {
            Ok(v) => Ok(Some(v)),
            Err(Error::NamespaceNotFound | Error::KeyNotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn wifi_ssid(&mut self) -> Result<Option<String>, Error> {
        self.get_string(&K_WIFI_SSID)
    }

    pub fn wifi_password(&mut self) -> Result<Option<String>, Error> {
        self.get_string(&K_WIFI_PASS)
    }

    pub fn image_url(&mut self) -> Result<Option<String>, Error> {
        self.get_string(&K_IMAGE_URL)
    }

    pub fn set_wifi_ssid(&mut self, v: &str) -> Result<(), Error> {
        self.nvs.set(&NS, &K_WIFI_SSID, v)
    }

    pub fn set_wifi_password(&mut self, v: &str) -> Result<(), Error> {
        self.nvs.set(&NS, &K_WIFI_PASS, v)
    }

    pub fn set_image_url(&mut self, v: &str) -> Result<(), Error> {
        self.nvs.set(&NS, &K_IMAGE_URL, v)
    }
}
