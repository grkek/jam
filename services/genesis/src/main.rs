//! Genesis, the root actor.
//!
//! With the `stress` feature this is the multi-actor EL0 messaging stress program.
//! The kernel boots many instances of this binary and sends each a bootstrap
//! message (`foundation::stress::Bootstrap`) naming a scenario. Healthy scenarios
//! verify messaging invariants and exit; fault scenarios die on purpose so the
//! kernel's isolation, cleanup and dead-address handling get exercised.
//!
//! Without the feature the kernel starts a single instance, which waits for
//! messages. This is where the system bootstrap will go.

#![no_std]
#![no_main]

#[cfg(feature = "stress")]
mod bootstrap;
#[cfg(feature = "stress")]
mod scenarios;
#[cfg(feature = "stress")]
mod support;

#[cfg(feature = "stress")]
use support::fail;

/// Receives the assignment, runs it and exits.
#[cfg(feature = "stress")]
fn run() -> ! {
    let mut record = [0u8; 128];
    let assignment = bootstrap::Assignment::receive(&mut record);

    scenarios::run(&assignment, &mut record);
    support::finish()
}

#[cfg(not(feature = "stress"))]
use foundation::tag::Tag;

/// Tag nobody sends, so waiting on it never completes.
#[cfg(not(feature = "stress"))]
const IDLE_TAG: Tag = Tag::new(0xdead, 0xbeef);

/// Waits for messages forever; there is nothing to bootstrap yet.
#[cfg(not(feature = "stress"))]
fn run() -> ! {
    use runtime::inbox::{self, MessageSelector};

    let mut record = [0u8; 128];
    loop {
        let selector = MessageSelector {
            tag: Some(IDLE_TAG),
            ..MessageSelector::default()
        };
        inbox::await_matching(selector, &mut record).unwrap_or_else(|_| fail());
    }
}

/// Stops through an actor-local fault; never reports successful completion.
#[cfg(not(feature = "stress"))]
fn fail() -> ! {
    // SAFETY: Intentional illegal instruction raises the actor-local failure path.
    unsafe {
        core::arch::asm!("brk #0", options(noreturn));
    }
}

/// Reports an actor-local failure instead of spinning indefinitely after a panic.
#[panic_handler]
fn on_panic(_information: &core::panic::PanicInfo<'_>) -> ! {
    fail()
}

/// Starts Genesis after the kernel enters this actor at EL0.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    run()
}
