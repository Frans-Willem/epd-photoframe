#![no_std]
extern crate alloc;

#[cfg(all(feature = "e1001", feature = "e1002"))]
compile_error!("features `e1001` and `e1002` are mutually exclusive");
#[cfg(all(feature = "e1001", feature = "e1004"))]
compile_error!("features `e1001` and `e1004` are mutually exclusive");
#[cfg(all(feature = "e1002", feature = "e1004"))]
compile_error!("features `e1002` and `e1004` are mutually exclusive");
#[cfg(not(any(feature = "e1001", feature = "e1002", feature = "e1004")))]
compile_error!("enable one of the device features: `e1001`, `e1002`, or `e1004`");

pub mod battery;
pub mod button;
pub mod buzzer;
pub mod canvas;
pub mod config;
pub mod config_image;
pub mod config_mode;
pub mod error_image;
pub mod gdep073e01;
pub mod gdey075t7;
pub mod grayscale;
pub mod hardware;
pub mod iter_util;
pub mod net_resources;
pub mod panel;
pub mod qr_image;
pub mod rtc_persisted;
pub mod sht40;
pub mod single_shot_wifi;
pub mod spectra6;
pub mod sy6974b;
pub mod t133a01;
pub mod url_util;
