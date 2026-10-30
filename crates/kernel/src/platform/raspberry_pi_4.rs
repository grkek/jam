//! Boot-time BCM2711 hardware layout for the Raspberry Pi 4 Model B.
//!
//! Requires `arm_peri_high=0`, `enable_gic=1`, and a firmware-configured PL011
//! UART0 on GPIO14/GPIO15. This first port intentionally initializes one core;
//! native Raspberry Pi secondary-core release is a separate platform service.
//!
//! Sources: BCM2711 ARM Peripherals, and Raspberry Pi firmware documentation.

use foundation::hardware_description::PhysicalRegion;

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
