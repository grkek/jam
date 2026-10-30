//! Selects the assembly boot entry matching the kernel's target platform.

#[cfg(not(feature = "raspberry-pi-4"))]
core::arch::global_asm!(include_str!("boot_entry.s"));

#[cfg(feature = "raspberry-pi-4")]
core::arch::global_asm!(include_str!("boot_entry_raspberry_pi_4.s"));
