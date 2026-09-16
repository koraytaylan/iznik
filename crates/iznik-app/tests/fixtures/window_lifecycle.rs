//! Fixed identities, geometry and process output for the real window lifecycle.

/// Session created through the window's production command bridge.
pub(crate) const SESSION: &str = "window-lifecycle";
/// Initial terminal width, changed by the rendered window's geometry.
pub(crate) const COLUMNS: u16 = 80;
/// Initial terminal height, changed by the rendered window's geometry.
pub(crate) const ROWS: u16 = 24;
/// Output marker before a transport loss, retained in the same client emulator.
pub(crate) const BEFORE: &str = "window-before-reconnect";
/// Output marker after transport recovery, appended to the retained emulator.
pub(crate) const AFTER: &str = "window-after-reconnect";
