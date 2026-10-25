//! First Rust code executed on the boot core.

use core::fmt::Write;

use crate::architecture::aarch64::{
    early_console::EarlyConsole,
    exceptions::{install_exception_vectors, trigger_test_breakpoint},
};

/// Called by the assembly entry once the stack is ready and `.bss` is cleared.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    install_exception_vectors();
    let _ = writeln!(EarlyConsole, "JAM: booted on the boot core");

    // Temporary: proves the exception vectors work. Remove once real faults exercise them.
    trigger_test_breakpoint();

    halt()
}

/// Parks the core forever.
pub fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
