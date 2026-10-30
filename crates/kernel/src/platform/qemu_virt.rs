//! Early hardware layout of QEMU's AArch64 `virt` machine.
//!
//! These virtual-board addresses must not be reused for physical boards.

use foundation::hardware_description::PhysicalRegion;

use super::EarlyDeviceMemoryRegion;

/// Physical base of QEMU `virt`'s first PL011 serial controller.
pub const PL011_UART_PHYSICAL_BASE_ADDRESS: usize = 0x0900_0000;
/// Physical base of QEMU `virt`'s GICv2 distributor.
pub const GIC_DISTRIBUTOR_PHYSICAL_BASE_ADDRESS: usize = 0x0800_0000;
/// Physical base of QEMU `virt`'s GICv2 CPU interface.
pub const GIC_CPU_INTERFACE_PHYSICAL_BASE_ADDRESS: usize = 0x0801_0000;

/// Boot-mapped physical RAM, preserved from the original QEMU configuration.
pub const BOOTSTRAP_MAPPED_RAM: PhysicalRegion = PhysicalRegion {
    start_address: 0x4000_0000,
    byte_count: 0x8000_0000,
};

/// Early 32 MiB QEMU interrupt-controller and UART MMIO window.
pub const EARLY_DEVICE_REGIONS: [EarlyDeviceMemoryRegion; 1] = [EarlyDeviceMemoryRegion {
    start_address: 0x0800_0000,
    end_address: 0x0a00_0000,
}];

/// QEMU `virt` also places its flattened device tree at the RAM base.
pub const DEVICE_TREE_FALLBACK_ADDRESS: Option<usize> = Some(0x4000_0000);

/// QEMU `virt` supports secondary-core startup through its PSCI interface.
pub const SECONDARY_CORE_STARTUP_SUPPORTED: bool = true;
