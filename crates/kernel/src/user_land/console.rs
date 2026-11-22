//! Place-holder user land starter.

use foundation::device_tree::PowerStateCoordinationInterfaceConduit;

/// Starts normal userland.
pub fn start(_conduit: Option<PowerStateCoordinationInterfaceConduit>) -> Result<(), &'static str> {
    Ok(())
}
