//! Pulls the boot assembly into the kernel image.

core::arch::global_asm!(include_str!("boot_entry.s"));
