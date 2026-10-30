//! Hardware-platform configuration for the JAM microkernel.
//!
//! A board is selected at build time. CPU instructions remain in
//! `architecture/aarch64`, while physical MMIO addresses and temporary mapping
//! extents live here. The QEMU `virt` board remains the default.

/// Physical device-memory interval needed during early boot.
///
/// Both bounds are physical addresses; the upper bound is exclusive. The
/// current block mapper requires both bounds to be aligned to 2 MiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EarlyDeviceMemoryRegion {
    /// First physical address mapped as Device memory.
    pub start_address: usize,
    /// First address beyond the Device memory region.
    pub end_address: usize,
}

/// Early hardware layout of QEMU's AArch64 `virt` board.
pub mod qemu_virt;
/// Early hardware layout of the Raspberry Pi 4 Model B (BCM2711).
pub mod raspberry_pi_4;

/// Platform selected for this kernel binary.
#[cfg(not(feature = "raspberry-pi-4"))]
pub use qemu_virt as current;
/// Platform selected for this kernel binary.
#[cfg(feature = "raspberry-pi-4")]
pub use raspberry_pi_4 as current;
