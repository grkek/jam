//! Hardware-platform configuration for the JAM microkernel.
//!
//! Architecture-specific instructions belong in `architecture/aarch64`; physical
//! addresses and hardware layout belong here. Early boot selects one platform
//! at build time. Device-tree discovery can eventually replace static values
//! after the kernel establishes safe initial memory mappings.

/// Physical device-memory interval required during early kernel initialization.
///
/// Both bounds are physical addresses; the upper bound is exclusive. During
/// the first portability phase, each interval must be aligned to a 2 MiB block,
/// because `kernel_map` maps early device windows with block descriptors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EarlyDeviceMemoryRegion {
    /// First physical address mapped with Device memory attributes.
    pub start_address: usize,
    /// First physical address beyond the mapped device region.
    pub end_address: usize,
}

/// Platform description for QEMU's AArch64 `virt` virtual motherboard.
pub mod qemu_virt;

/// Selected platform for this build; currently QEMU `virt` only.
///
/// A later step will make this alias selectable at build time while keeping
/// users of `current` independent of the platform's name.
pub use qemu_virt as current;
