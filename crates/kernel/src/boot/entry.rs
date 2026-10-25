//! First Rust code executed on the boot core.

use core::fmt::Write;

use crate::architecture::aarch64::early_console::EarlyConsole;

/// Called by the assembly entry once the stack is ready and `.bss` is cleared.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    let _ = writeln!(EarlyConsole, "JAM: booted on the boot core");
    halt()
}

/// Parks the core forever.
pub fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
