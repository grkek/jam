//! Early hardware layout of QEMU's AArch64 `virt` machine.
//!
//! These addresses describe the virtual motherboard, not the AArch64 CPU.
//! Do not reuse them when porting JAM to a physical board.

use super::EarlyDeviceMemoryRegion;
use foundation::hardware_description::PhysicalRegion;

/// Physical base of QEMU `virt`'s first PL011 serial controller.
pub const PL011_UART_PHYSICAL_BASE_ADDRESS: usize = 0x0900_0000;

/// Physical base of the QEMU `virt` GICv2 distributor.
pub const GIC_DISTRIBUTOR_PHYSICAL_BASE_ADDRESS: usize = 0x0800_0000;

/// Physical base of the QEMU `virt` GICv2 CPU interface.
pub const GIC_CPU_INTERFACE_PHYSICAL_BASE_ADDRESS: usize = 0x0801_0000;

/// Early MMIO windows required by the kernel before device discovery.
///
/// This preserves JAM's existing 32 MiB QEMU device mapping exactly. The
/// interval is intentionally 2 MiB-aligned for the boot-era block mapper.
/// RAM accessible through the temporary TTBR1 1 GiB block translations.
/// The strict kernel map later replaces these coarse translations.
pub const BOOTSTRAP_MAPPED_RAM: PhysicalRegion = PhysicalRegion {
    start_address: 0x4000_0000,
    byte_count: 0x8000_0000,
};

/// Early MMIO regions covering QEMU `virt`'s interrupt controller and UART.
pub const EARLY_DEVICE_REGIONS: [EarlyDeviceMemoryRegion; 1] = [EarlyDeviceMemoryRegion {
    start_address: 0x0800_0000,
    end_address: 0x0a00_0000,
}];
