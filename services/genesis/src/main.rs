//! Genesis: the root actor responsible for
//! bootstrapping and supervising the system.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[panic_handler]
fn panic_handler(_: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
/// :ditto:
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
