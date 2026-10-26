//! The actor microkernel for `AArch64`.

#![no_std]
#![no_main]

pub mod actors;
pub mod architecture;
pub mod boot;
pub mod memory;
pub mod messaging;
pub mod observation;
pub mod scheduling;
pub mod synchronization;
pub mod system_calls;

use core::fmt::Write;

use architecture::aarch64::early_console::EarlyConsole;
use foundation::device_tree::PowerStateCoordinationInterfaceConduit;

use crate::architecture::aarch64::power_interface;

/// Reports the panic on the serial console, then parks the core.
#[panic_handler]
fn on_kernel_panic(panic_information: &core::panic::PanicInfo) -> ! {
    let _ = writeln!(EarlyConsole, "\nKERNEL PANIC: {panic_information}");
    power_interface::system_off(PowerStateCoordinationInterfaceConduit::SecureMonitorCall)
}
