//! `'static` storage for the embassy-net `StackResources`. Both
//! `main_normal` and `config_mode::run` reach into it directly; only
//! one mode runs per boot, so a single instance is enough and the
//! other mode's reference simply never executes.
//!
//! `StackResources` is sized for the worst case (config mode: 1 DHCP
//! server + 1 DNS hijack + 1 HTTP listener + the HTTP handler-task
//! pool + slack so probe bursts don't panic smoltcp). Normal mode
//! carries the extra slots but never uses them — each slot is smoltcp
//! socket metadata only (a few hundred bytes), so the cost is ~1 KB
//! of `.bss` we wouldn't otherwise need.

/// Number of embassy-net socket slots. Max across both modes — see
/// the module-level doc for the per-mode breakdown.
pub const NUM_SOCKETS: usize = 10;

pub static NETWORK_RESOURCES: static_cell::ConstStaticCell<
    embassy_net::StackResources<NUM_SOCKETS>,
> = static_cell::ConstStaticCell::new(embassy_net::StackResources::new());
