//! Minimal PL011 serial output for early boot and panic diagnostics.
//!
//! The firmware must select and configure the UART before entering JAM.
//! The production console will be a driver actor. Writes are serialized to
//! prevent interleaved log records when multiprocessing is enabled.

use core::fmt;

use crate::{
    memory::address_translation::physical_to_virtual, synchronization::spin_lock::SpinLock,
};

/// Virtual address of the platform-selected PL011 register block.
const PL011_BASE_ADDRESS: usize =
    physical_to_virtual(crate::platform::current::PL011_UART_PHYSICAL_BASE_ADDRESS);
/// Serializes formatted log records across execution cores.
static CONSOLE_LOCK: SpinLock<()> = SpinLock::new(());

/// Early platform-selected PL011 UART console.
pub struct EarlyConsole;

impl EarlyConsole {
    /// Sends one byte after space becomes available in the transmit FIFO.
    fn write_byte(byte: u8) {
        // SAFETY: Boot maps the platform-selected register block as Device memory.
        // The caller holds CONSOLE_LOCK across formatted or raw output.
        unsafe {
            pl011_console::write_byte(PL011_BASE_ADDRESS, byte);
        }
    }

    /// Serializes terminal output with kernel logs, translating newlines to CRLF.
    pub fn write_bytes(bytes: &[u8]) {
        let _guard = CONSOLE_LOCK.lock();
        for &byte in bytes {
            if byte == b'\n' {
                Self::write_byte(b'\r');
            }
            Self::write_byte(byte);
        }
    }
}

/// Unlocked writer used only while the public console owns the output lock.
struct LockedWriter;

impl fmt::Write for LockedWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if byte == b'\n' {
                EarlyConsole::write_byte(b'\r');
            }
            EarlyConsole::write_byte(byte);
        }
        Ok(())
    }
}

impl fmt::Write for EarlyConsole {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        Self::write_bytes(text.as_bytes());
        Ok(())
    }

    fn write_fmt(&mut self, arguments: fmt::Arguments<'_>) -> fmt::Result {
        let _guard = CONSOLE_LOCK.lock();
        fmt::write(&mut LockedWriter, arguments)
    }
}
