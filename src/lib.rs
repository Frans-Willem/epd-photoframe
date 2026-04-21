#![no_std]
extern crate alloc;

#[cfg(all(feature = "e1002", feature = "e1004"))]
compile_error!("features `e1002` and `e1004` are mutually exclusive");
#[cfg(not(any(feature = "e1002", feature = "e1004")))]
compile_error!("enable one of the device features: `e1002` or `e1004`");

pub mod config;
pub mod error_image;
pub mod gdep073e01;
pub mod spectra6;
pub mod t133a01;
