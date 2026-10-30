//! Minimal PL011 serial output for early boot and panic diagnostics.
//!
//! The firmware must select and configure the UART before entering JAM.
//! The production console will be a driver actor. Writes are serialized to
//! prevent interleaved log records when multiprocessing is enabled.

use core::{
    fmt,
    ptr::{read_volatile, write_volatile},
};

use crate::{
    memory::address_translation::physical_to_virtual, synchronization::spin_lock::SpinLock,
};

/// Virtual address of the platform-selected PL011 register block.
const PL011_BASE_ADDRESS: usize =
    physical_to_virtual(crate::platform::current::PL011_UART_PHYSICAL_BASE_ADDRESS);
/// Offset of the PL011 data register.
const DATA_REGISTER_OFFSET: usize = 0x00;
/// Offset of the PL011 flag register.
const FLAG_REGISTER_OFFSET: usize = 0x18;
/// Flag identifying a full transmit FIFO.
const TRANSMIT_FIFO_FULL: u32 = 1 << 5;

/// Serializes formatted log records across execution cores.
static CONSOLE_LOCK: SpinLock<()> = SpinLock::new(());

/// Early platform-selected PL011 UART console.
pub struct EarlyConsole;

impl EarlyConsole {
    /// Sends one byte after space becomes available in the transmit FIFO.
    fn write_byte(byte: u8) {
        let flag_register = (PL011_BASE_ADDRESS + FLAG_REGISTER_OFFSET) as *const u32;
        let data_register = (PL011_BASE_ADDRESS + DATA_REGISTER_OFFSET) as *mut u32;

        // SAFETY: Both register addresses come from the selected board's
        // 32-bit-aligned PL011 MMIO window, which boot maps as Device memory.
        // Volatile operations are required to access the UART registers.
        unsafe {
            while read_volatile(flag_register) & TRANSMIT_FIFO_FULL != 0 {
                // A draining UART FIFO need not generate an architectural
                // event: `wfe` here could sleep indefinitely on real hardware.
                core::hint::spin_loop();
            }
            write_volatile(data_register, u32::from(byte));
        }
    }
}

impl fmt::Write for EarlyConsole {
    /// Sends text with CRLF line endings.
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if byte == b'\n' {
                Self::write_byte(b'\r');
            }
            Self::write_byte(byte);
        }
        Ok(())
    }

    /// Holds the console lock across one formatted message.
    fn write_fmt(&mut self, arguments: fmt::Arguments<'_>) -> fmt::Result {
        let _guard = CONSOLE_LOCK.lock();
        fmt::write(self, arguments)
    }
}
