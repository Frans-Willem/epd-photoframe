//! Small UART helpers. Currently just one — a hardware-shift-register
//! drain that callers run before `rtc.sleep_deep(..)` or
//! `software_reset()`, since both cut the UART peripheral clock and
//! truncate any bytes still mid-flight.

/// Block until the UART0 TX path has fully clocked out everything
/// `esp-println` handed it. Polls the FIFO byte count down to zero,
/// waits 10 µs for the FSM to transition to idle (the esp-hal driver
/// does the same fixup — the FSM can briefly stay busy after the last
/// byte leaves the FIFO), then polls the "transmit FSM is idle" status
/// bit. Mirrors `esp_hal::uart::UartTx::flush` but operates on the PAC
/// directly so we don't need to steal UART0 from `esp-println`.
///
/// Used right before `rtc.sleep_deep(..)` and `software_reset()`: both
/// cut the UART peripheral clock, so any bytes still in the shift
/// register when we arrive there are truncated on the wire.
/// `esp-println`'s ROM TX_FLUSH drains the software FIFO but *not* the
/// hardware shift register.
pub fn wait_for_tx_idle() {
    let uart0 = esp_hal::peripherals::UART0::regs();
    while uart0.status().read().txfifo_cnt().bits() > 0 {}
    esp_hal::delay::Delay::new().delay_micros(10);
    while uart0.fsm_status().read().st_utx_out().bits() != 0 {}
}
