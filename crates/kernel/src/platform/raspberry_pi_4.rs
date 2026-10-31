//! Boot-time BCM2711 hardware layout for the Raspberry Pi 4 Model B.
//!
//! Requires `arm_peri_high=0`, `enable_gic=1`, and a firmware-configured PL011
//! UART0 on GPIO14/GPIO15. This first port intentionally initializes one core;
//! native Raspberry Pi secondary-core release is a separate platform service.
//!
//! Sources: BCM2711 ARM Peripherals, and Raspberry Pi firmware documentation.

use foundation::hardware_description::PhysicalRegion;

use crate::memory::address_translation::physical_to_virtual;

use super::EarlyDeviceMemoryRegion;

/// ARM physical base of PL011 UART0, provided by the BCM2711 peripheral bus.
pub const PL011_UART_PHYSICAL_BASE_ADDRESS: usize = 0xfe20_1000;
/// ARM physical base of the GIC-400 distributor in low-peripheral mode.
pub const GIC_DISTRIBUTOR_PHYSICAL_BASE_ADDRESS: usize = 0xff84_1000;
/// ARM physical base of the GIC-400 CPU interface in low-peripheral mode.
pub const GIC_CPU_INTERFACE_PHYSICAL_BASE_ADDRESS: usize = 0xff84_2000;

/// Initial normal-memory mapping, limited to the first 1 GiB of physical RAM.
///
/// The allocator must not manage addresses outside this interval until JAM
/// supports dynamic boot mappings for larger or discontiguous RAM banks.
pub const BOOTSTRAP_MAPPED_RAM: PhysicalRegion = PhysicalRegion {
    start_address: 0x0000_0000,
    byte_count: 0x4000_0000,
};

/// Low-peripheral-mode MMIO window containing UART0 and both GIC interfaces.
///
/// The 32 MiB interval is aligned for JAM's 2 MiB device-block mapper.
pub const EARLY_DEVICE_REGIONS: [EarlyDeviceMemoryRegion; 1] = [EarlyDeviceMemoryRegion {
    start_address: 0xfe00_0000,
    end_address: 0x1_0000_0000,
}];

/// The Pi firmware must pass the flattened device tree address in register x0.
///
/// Blindly probing a hard-coded address on real hardware can fault or read
/// firmware-owned memory. Unlike QEMU, no fallback address is assumed.
pub const DEVICE_TREE_FALLBACK_ADDRESS: Option<usize> = None;

/// Prevents native secondary-core startup until BCM2711 release is implemented.
pub const SECONDARY_CORE_STARTUP_SUPPORTED: bool = false;

///
pub fn initialise_console_hardware() {
    const GPIO: usize = physical_to_virtual(0xfe20_0000);
    const UART: usize = physical_to_virtual(PL011_UART_PHYSICAL_BASE_ADDRESS);
    // SAFETY: both blocks sit inside the Device-mapped early window.
    unsafe {
        // GPIO14/15 -> ALT0 (PL011 TXD0/RXD0), no pull-up/down (BCM2711 PUP_PDN regs)
        let sel = (GPIO + 0x04) as *mut u32;
        core::ptr::write_volatile(
            sel,
            (core::ptr::read_volatile(sel) & !((7 << 12) | (7 << 15))) | (4 << 12) | (4 << 15),
        );
        let pull = (GPIO + 0xe4) as *mut u32;
        core::ptr::write_volatile(
            pull,
            core::ptr::read_volatile(pull) & !((3 << 28) | (3 << 30)),
        );

        // PL011 at 48 MHz, 115200 8N1, FIFOs on
        core::ptr::write_volatile((UART + 0x30) as *mut u32, 0); // CR: disable
        while core::ptr::read_volatile((UART + 0x18) as *const u32) & (1 << 3) != 0 {} // wait !BUSY
        core::ptr::write_volatile((UART + 0x44) as *mut u32, 0x7ff); // ICR: clear
        core::ptr::write_volatile((UART + 0x24) as *mut u32, 26); // IBRD
        core::ptr::write_volatile((UART + 0x28) as *mut u32, 3); // FBRD
        core::ptr::write_volatile((UART + 0x2c) as *mut u32, 0x70); // LCRH: 8 bits, FIFO
        core::ptr::write_volatile((UART + 0x30) as *mut u32, 0x301); // CR: UARTEN|TXE|RXE
    }
}
