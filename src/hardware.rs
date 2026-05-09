//! Shared hardware-adjacent app types.

/// What the user (or the wake timer) wants us to do this cycle. Consumed
/// by the normal flow to pick an `action=` query-string fragment and to
/// decide whether to run the white pre-flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeAction {
    /// Fresh power-on / cold reset. Show the white pre-flash, fetch with no action query.
    FreshBoot,
    /// Woken by the 10-minute timer. No pre-flash, fetch with no action query.
    Timer,
    /// Woken by the Refresh button (or by a GPIO wake where no button is still held).
    Refresh,
    Previous,
    Next,
}

impl WakeAction {
    /// Name of the `action=` query-string parameter to append to the
    /// base URL, or `None` if the base URL should be fetched unchanged.
    /// The caller is responsible for picking the right `?` / `&`
    /// separator depending on whether the base URL already carries a
    /// query string.
    pub fn action_name(self) -> Option<&'static str> {
        match self {
            WakeAction::Refresh => Some("refresh"),
            WakeAction::Previous => Some("previous"),
            WakeAction::Next => Some("next"),
            WakeAction::FreshBoot | WakeAction::Timer => None,
        }
    }

    /// Whether to trigger a non-blocking white pre-flash as immediate visual
    /// feedback that the press was seen. The 10-minute timer wake runs in the
    /// background, so no feedback is needed there.
    pub fn show_white_flash(self) -> bool {
        !matches!(self, WakeAction::Timer)
    }
}
