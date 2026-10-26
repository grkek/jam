//! `AArch64`: the only home of assembly and system register access.

pub mod boot_entry;
pub mod early_console;
pub mod exceptions;
pub mod generic_timer;
pub mod interrupt_controller;
pub mod memory_management_unit;
pub mod power_interface;
pub mod processor;
pub mod time;
