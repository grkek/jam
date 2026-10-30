//! The actor-oriented microkernel for AArch64.
//!
//! Hardware-specific addresses are selected through `platform`, while the
//! memory manager, scheduler, actors, and system calls stay board-independent.

#![no_std]
#![no_main]

/// Actor management, spawning, and lifecycle cleanup.
pub mod actors;
/// CPU architecture and low-level exception machinery.
pub mod architecture;
/// Kernel boot and startup orchestration.
pub mod boot;
/// Physical and virtual memory management.
pub mod memory;
/// Actor message transport and bookkeeping.
pub mod messaging;
/// Kernel event collection and diagnostic observability.
pub mod observation;
/// Early physical hardware configuration for the selected platform.
pub mod platform;
/// Per-core execution and actor scheduling.
pub mod scheduling;
/// Synchronization primitives shared by kernel cores.
pub mod synchronization;
/// User-to-kernel system-call dispatch and handlers.
pub mod system_calls;

use core::fmt::Write;

use architecture::aarch64::{early_console::EarlyConsole, power_interface};

/// Reports a panic on the serial console before stopping the machine.
#[panic_handler]
fn on_kernel_panic(panic_information: &core::panic::PanicInfo) -> ! {
    let _ = writeln!(EarlyConsole, "\nKERNEL PANIC: {panic_information}");
    power_interface::halt_machine()
}
