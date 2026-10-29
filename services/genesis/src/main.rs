//! Genesis: the first userland actor, and in time the main userland steward.

#![no_std]
#![no_main]

use core::{hint::spin_loop, panic::PanicInfo};

/// A panic parks Genesis. Ending itself with a cause comes with the runtime library.
#[panic_handler]
fn on_panic(_panic_information: &PanicInfo) -> ! {
    park()
}

/// Where the kernel's loader starts the actor.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    park()
}

fn park() -> ! {
    loop {
        spin_loop();
    }
}
