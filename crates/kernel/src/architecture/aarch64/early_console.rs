//! Minimal PL011 serial output for the earliest boot and for panics.
//!
//! This is a boot aid, not the driver. The real console is a service actor.

use core::{
    fmt,
    ptr::{read_volatile, write_volatile},
};

/// Where QEMU `virt` maps the first PL011 UART.
const PL011_BASE_ADDRESS: usize = 0x0900_0000;
const DATA_REGISTER_OFFSET: usize = 0x00;
const FLAG_REGISTER_OFFSET: usize = 0x18;
/// Flag register bit that is set while the transmit FIFO is full.
const TRANSMIT_FIFO_FULL: u32 = 1 << 5;

/// Writes text to the first PL011 UART.
pub struct EarlyConsole;

impl EarlyConsole {
    fn write_byte(byte: u8) {
        let flag_register = (PL011_BASE_ADDRESS + FLAG_REGISTER_OFFSET) as *const u32;
        let data_register = (PL011_BASE_ADDRESS + DATA_REGISTER_OFFSET) as *mut u32;

        // SAFETY: The PL011 registers are memory-mapped at a fixed physical address on QEMU
        // `virt`, and the MMU is still off, so physical and virtual addresses are the same.
        // Both registers are 32 bits wide and aligned, and volatile access is how devices are read.
        unsafe {
            while read_volatile(flag_register) & TRANSMIT_FIFO_FULL != 0 {
                core::hint::spin_loop();
            }

            write_volatile(data_register, u32::from(byte));
        }
    }
}

impl fmt::Write for EarlyConsole {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if byte == b'\n' {
                Self::write_byte(b'\r');
            }
            Self::write_byte(byte);
        }
        Ok(())
    }
}
