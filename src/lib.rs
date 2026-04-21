#![no_std]
extern crate alloc;

#[cfg(all(feature = "e1002", feature = "e1004"))]
compile_error!("features `e1002` and `e1004` are mutually exclusive");
#[cfg(not(any(feature = "e1002", feature = "e1004")))]
compile_error!("enable one of the device features: `e1002` or `e1004`");

pub mod canvas;
pub mod config;
pub mod config_image;
pub mod config_mode;
pub mod error_image;
pub mod gdep073e01;
pub mod hardware;
pub mod portal;
pub mod qr_image;
pub mod spectra6;
pub mod t133a01;
