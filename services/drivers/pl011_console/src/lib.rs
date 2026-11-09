//! Minimal PL011 byte access used by the polling console driver actor.
//!
//! Device ownership and mapping remain the kernel's responsibility until driver
//! capabilities are implemented. This crate does not parse terminal commands.

#![no_std]

use core::ptr::{read_volatile, write_volatile};

/// Attempts to read one byte without waiting for input.
/// Framing, parity, break, and overrun errors are cleared and discarded.
///
/// # Safety
/// `base_address` must name a mapped, aligned PL011 Device-memory register block.
/// The caller must exclusively own receive operations for this peripheral.
#[must_use]
pub unsafe fn read_byte(base_address: usize) -> Option<u8> {
    // SAFETY: The caller supplies a valid Device mapping and exclusive receive access.
    unsafe {
        if read_volatile((base_address + 0x18) as *const u32) & (1 << 4) != 0 {
            return None;
        }
        let data = read_volatile(base_address as *const u32);
        if data & 0x0f00 != 0 {
            write_volatile((base_address + 0x04) as *mut u32, 0);
            return None;
        }
        Some(data.to_le_bytes()[0])
    }
}

/// Writes one byte, waiting only for transmit space.
///
/// # Safety
/// `base_address` must name a mapped, aligned PL011 Device-memory register block.
/// The caller must serialize writes with other users of this peripheral.
pub unsafe fn write_byte(base_address: usize, byte: u8) {
    // SAFETY: The caller supplies a valid Device mapping and serialized transmit access.
    unsafe {
        while read_volatile((base_address + 0x18) as *const u32) & (1 << 5) != 0 {
            core::hint::spin_loop();
        }
        write_volatile(base_address as *mut u32, u32::from(byte));
    }
}
