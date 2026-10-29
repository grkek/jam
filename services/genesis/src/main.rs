//! Genesis: the first userland actor, and in time the main userland steward.

#![no_std]
#![no_main]

use core::{arch::asm, hint::spin_loop, panic::PanicInfo};
use foundation::system_call::SystemCall;

/// A panic parks Genesis. Ending itself with a cause comes with the runtime library.
#[panic_handler]
fn on_panic(_panic_information: &PanicInfo) -> ! {
    park()
}

/// Where the kernel's loader starts the actor.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    finish()
}

fn finish() -> ! {
    // SAFETY: `FinishActor` never returns to the caller.
    unsafe {
        asm!("svc #0", in("x8") SystemCall::FinishActor as u64, options(noreturn));
    }
}

fn park() -> ! {
    loop {
        spin_loop();
    }
}
