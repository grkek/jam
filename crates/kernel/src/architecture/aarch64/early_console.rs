//! Minimal PL011 serial output for the earliest boot and for panics.
//!
//! This is a boot aid, not the driver. The real console is a service actor.
//! Each formatted write holds a lock, so lines from different cores never interleave.

use core::{
    fmt,
    ptr::{read_volatile, write_volatile},
};

use crate::synchronization::spin_lock::SpinLock;
use crate::{
    architecture::aarch64::processor::wait_for_event,
    memory::address_translation::physical_to_virtual,
};

/// Kernel virtual address of the selected platform's first PL011 UART.
const PL011_BASE_ADDRESS: usize =
    physical_to_virtual(crate::platform::current::PL011_UART_PHYSICAL_BASE_ADDRESS);
/// Offset of the PL011 data register.
const DATA_REGISTER_OFFSET: usize = 0x00;
/// Offset of the PL011 flags register.
const FLAG_REGISTER_OFFSET: usize = 0x18;
/// Flag register bit that is set while the transmit FIFO is full.
const TRANSMIT_FIFO_FULL: u32 = 1 << 5;

/// Prevents interleaving formatted messages across cores.
static CONSOLE_LOCK: SpinLock<()> = SpinLock::new(());

/// Writes text to the first PL011 UART.
pub struct EarlyConsole;

impl EarlyConsole {
    /// Sends one byte to the UART.
    fn write_byte(byte: u8) {
        let flag_register = (PL011_BASE_ADDRESS + FLAG_REGISTER_OFFSET) as *const u32;
        let data_register = (PL011_BASE_ADDRESS + DATA_REGISTER_OFFSET) as *mut u32;

        // SAFETY: The PL011 registers are memory-mapped at a fixed physical address on QEMU
        // `virt`, mapped as Device memory once the MMU is on. Both registers are 32 bits
        // wide and aligned, and volatile access is how devices are read.
        unsafe {
            while read_volatile(flag_register) & TRANSMIT_FIFO_FULL != 0 {
                wait_for_event();
            }
            write_volatile(data_register, u32::from(byte));
        }
    }
}

impl fmt::Write for EarlyConsole {
    /// Sends serial text using CRLF line endings.
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if byte == b'\n' {
                Self::write_byte(b'\r');
            }
            Self::write_byte(byte);
        }
        Ok(())
    }

    /// Serializes a complete formatted output message.
    fn write_fmt(&mut self, arguments: fmt::Arguments<'_>) -> fmt::Result {
        let _guard = CONSOLE_LOCK.lock();
        fmt::write(self, arguments)
    }
}
