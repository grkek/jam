//! Multi-actor EL0 messaging stress program.
//!
//! The kernel boots many instances of this binary and sends each a bootstrap
//! message (`foundation::stress::Bootstrap`) naming a scenario. Healthy scenarios
//! verify messaging invariants and exit; fault scenarios die on purpose so the
//! kernel's isolation, cleanup and dead-address handling get exercised.

#![no_std]
#![no_main]

mod bootstrap;
mod scenarios;
mod support;

use bootstrap::Assignment;

/// Receives the assignment, runs it and exits.
fn run() -> ! {
    let mut record = [0u8; 128];
    let assignment = Assignment::receive(&mut record);

    scenarios::run(&assignment, &mut record);
    support::finish()
}

/// Reports an actor-local failure instead of spinning indefinitely after a panic.
#[panic_handler]
fn on_panic(_information: &core::panic::PanicInfo<'_>) -> ! {
    support::fail()
}

/// Starts the stress program after the kernel enters this actor at EL0.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    run()
}
