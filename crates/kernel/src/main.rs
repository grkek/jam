//! The actor microkernel for AArch64.

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

/// First instruction stream executed after firmware hands over control.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    halt()
}

/// Parks the core forever.
fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// Placeholder until the observation layer can record the fault.
#[panic_handler]
fn on_kernel_panic(_panic_information: &core::panic::PanicInfo) -> ! {
    halt()
}
