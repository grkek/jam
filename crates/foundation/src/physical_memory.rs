//! Validates early physical RAM ownership and plans a reservation-safe frame bitmap.
//!
//! The first AArch64 bootstrap maps only one physical RAM bank. This module deliberately
//! manages that bank alone until JAM can establish mappings for discontiguous banks.
//! Additional banks are checked for overlap, but are never silently treated as mapped.
//! Every reservation is rounded outward to complete 4 KiB frames before allocation.
//! This module has no hardware dependencies, allocations, or unsafe code.

use crate::hardware_description::{
    MAXIMUM_RESERVED_REGIONS, PhysicalRegion, ReservationSource, ReservedRegion,
};

/// Physical frame granularity used by JAM and `AArch64` page tables.
pub const PHYSICAL_FRAME_BYTE_COUNT: u64 = 4096;
/// Current bootstrap's 2 MiB block granularity for RAM mappings.
const BOOTSTRAP_BLOCK_BYTE_COUNT: u64 = 2 * 1024 * 1024;
/// Extra reservations for the boot image, DTB, and bitmap storage.
const EXTRA_PROTECTED_REGION_COUNT: usize = 3;
/// Maximum number of tracked physical reservations during early boot.
pub const MAXIMUM_PROTECTED_REGIONS: usize =
    MAXIMUM_RESERVED_REGIONS + EXTRA_PROTECTED_REGION_COUNT;

/// An error that makes unsafe physical memory allocation impossible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalMemoryError {
    /// The legacy boot RAM bank was not present exactly in hardware discovery.
    ManagedBankMissing,
    /// A RAM bank extends beyond the bootstrap translation tables' accessible RAM.
    OutsideBootstrapMapping,
    /// Two firmware RAM ranges overlap or contain a malformed extent.
    OverlappingMemoryBanks,
    /// Current bootstrap cannot map a bank whose edges are not 2 MiB aligned.
    UnsupportedBootstrapAlignment,
    /// The loaded kernel ends outside the RAM bank selected for this bootstrap.
    KernelOutsideManagedBank,
    /// A dynamic reservation request has not been satisfied by firmware or JAM.
    UnresolvedDynamicReservation,
    /// A `no-map` reservation intersects a region the temporary boot tables already map.
    UnsupportedNoMapReservation,
    /// There is insufficient space for the frame bitmap outside protected memory.
    BitmapStorageUnavailable,
    /// A length or address computation would overflow.
    AddressOverflow,
    /// The supplied reservation list exceeds early boot capacity.
    TooManyReservations,
    /// The device-tree blob extent is empty or otherwise invalid.
    InvalidDeviceTreeRegion,
}

/// The managed RAM bank, storage for its bitmap, and permanently protected extents.
///
/// All protected regions may extend outside managed RAM; the frame allocator intersects them
/// with the managed bank when marking used bits. A frame is never freeable if it touches one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalMemoryPlan {
    managed_bank: PhysicalRegion,
    bitmap_storage: PhysicalRegion,
    protected_regions: [PhysicalRegion; MAXIMUM_PROTECTED_REGIONS],
    protected_region_count: usize,
}

/// Rounds an address upward to a power-of-two alignment without overflowing.
fn align_up(value: u64, alignment: u64) -> Option<u64> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|address| address & !(alignment - 1))
}

/// Returns whether two nonempty half-open physical ranges share any byte.
fn regions_overlap(left: PhysicalRegion, right: PhysicalRegion) -> bool {
    left.start_address < right.end_address() && right.start_address < left.end_address()
}

/// Collects immutable reservations and boot allocations before placing the bitmap.
///
/// Checked writes enforce the fixed array capacity even when a caller supplies
/// an inconsistent number of firmware reservation entries.
fn collect_protected_regions(
    reservations: &[ReservedRegion],
    managed_bank: PhysicalRegion,
    kernel_end: u64,
    device_tree_region: PhysicalRegion,
) -> Result<([PhysicalRegion; MAXIMUM_PROTECTED_REGIONS], usize), PhysicalMemoryError> {
    let mut protected_regions = [PhysicalRegion {
        start_address: 0,
        byte_count: 0,
    }; MAXIMUM_PROTECTED_REGIONS];
    let mut protected_region_count = 0_usize;
    for reservation in reservations {
        // Refuse an overflowing inventory instead of indexing beyond its fixed storage.
        let destination = protected_regions
            .get_mut(protected_region_count)
            .ok_or(PhysicalMemoryError::TooManyReservations)?;
        *destination = reservation.region;
        protected_region_count += 1;
    }
    // Protect every page before the end of the loaded kernel, including boot-time tables.
    let kernel_region = protected_regions
        .get_mut(protected_region_count)
        .ok_or(PhysicalMemoryError::TooManyReservations)?;
    *kernel_region = PhysicalRegion {
        start_address: managed_bank.start_address,
        byte_count: kernel_end - managed_bank.start_address,
    };
    protected_region_count += 1;
    let device_tree_destination = protected_regions
        .get_mut(protected_region_count)
        .ok_or(PhysicalMemoryError::TooManyReservations)?;
    *device_tree_destination = device_tree_region;
    protected_region_count += 1;
    Ok((protected_regions, protected_region_count))
}

/// Finds a page-aligned bitmap interval that does not intersect protected memory.
///
/// Moves the candidate to the end of the furthest intersecting reservation and
/// fails if the requested storage cannot fit in the already-mapped RAM bank.
fn find_bitmap_storage(
    managed_bank: PhysicalRegion,
    kernel_end: u64,
    bitmap_storage_bytes: u64,
    protected_regions: &[PhysicalRegion],
) -> Result<PhysicalRegion, PhysicalMemoryError> {
    let mut candidate = align_up(kernel_end, PHYSICAL_FRAME_BYTE_COUNT)
        .ok_or(PhysicalMemoryError::AddressOverflow)?;
    loop {
        let end = candidate
            .checked_add(bitmap_storage_bytes)
            .ok_or(PhysicalMemoryError::AddressOverflow)?;
        if end > managed_bank.end_address() {
            return Err(PhysicalMemoryError::BitmapStorageUnavailable);
        }
        let proposed = PhysicalRegion {
            start_address: candidate,
            byte_count: bitmap_storage_bytes,
        };
        let mut next_candidate = candidate;
        for region in protected_regions {
            if regions_overlap(proposed, *region) {
                let aligned_end = align_up(region.end_address(), PHYSICAL_FRAME_BYTE_COUNT)
                    .ok_or(PhysicalMemoryError::AddressOverflow)?;
                next_candidate = next_candidate.max(aligned_end);
            }
        }
        if next_candidate == candidate {
            return Ok(proposed);
        }
        candidate = next_candidate;
    }
}

impl PhysicalMemoryPlan {
    /// Builds an early allocation plan from *validated* discovery and boot image extents.
    ///
    /// `managed_bank` must match one complete discovered RAM bank. Extra banks are validated
    /// but remain unavailable until the kernel maps them. The bitmap is placed at the first
    /// suitable page boundary after the kernel image, skipping the DTB and reservations.
    ///
    /// # Errors
    ///
    /// Returns an explicit error instead of making any RAM available when the discovery is
    /// inconsistent, a reservation cannot be honored, or the bitmap has no safe location.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        managed_bank: PhysicalRegion,
        bootstrap_mapped_ram: PhysicalRegion,
        discovered_banks: &[PhysicalRegion],
        reservations: &[ReservedRegion],
        dynamic_reservation_count: usize,
        kernel_end: u64,
        device_tree_region: PhysicalRegion,
        bitmap_storage_byte_count: u64,
    ) -> Result<Self, PhysicalMemoryError> {
        if dynamic_reservation_count != 0 {
            return Err(PhysicalMemoryError::UnresolvedDynamicReservation);
        }
        if PhysicalRegion::new(
            device_tree_region.start_address,
            device_tree_region.byte_count,
        )
        .is_none()
        {
            return Err(PhysicalMemoryError::InvalidDeviceTreeRegion);
        }
        if PhysicalRegion::new(managed_bank.start_address, managed_bank.byte_count).is_none()
            || PhysicalRegion::new(
                bootstrap_mapped_ram.start_address,
                bootstrap_mapped_ram.byte_count,
            )
            .is_none()
        {
            return Err(PhysicalMemoryError::AddressOverflow);
        }
        if managed_bank.start_address < bootstrap_mapped_ram.start_address
            || managed_bank.end_address() > bootstrap_mapped_ram.end_address()
        {
            return Err(PhysicalMemoryError::OutsideBootstrapMapping);
        }
        if discovered_banks
            .iter()
            .filter(|region| **region == managed_bank)
            .count()
            != 1
        {
            return Err(PhysicalMemoryError::ManagedBankMissing);
        }
        for (index, first) in discovered_banks.iter().enumerate() {
            if PhysicalRegion::new(first.start_address, first.byte_count).is_none() {
                return Err(PhysicalMemoryError::OverlappingMemoryBanks);
            }
            for second in discovered_banks.iter().skip(index + 1) {
                if regions_overlap(*first, *second) {
                    return Err(PhysicalMemoryError::OverlappingMemoryBanks);
                }
            }
        }
        // The current kernel map rounds RAM to 2 MiB blocks, so partial blocks are unsafe.
        if managed_bank.start_address % BOOTSTRAP_BLOCK_BYTE_COUNT != 0
            || managed_bank.end_address() % BOOTSTRAP_BLOCK_BYTE_COUNT != 0
        {
            return Err(PhysicalMemoryError::UnsupportedBootstrapAlignment);
        }
        if kernel_end <= managed_bank.start_address || kernel_end > managed_bank.end_address() {
            return Err(PhysicalMemoryError::KernelOutsideManagedBank);
        }
        if reservations.len() > MAXIMUM_RESERVED_REGIONS {
            return Err(PhysicalMemoryError::TooManyReservations);
        }
        for reservation in reservations {
            if PhysicalRegion::new(
                reservation.region.start_address,
                reservation.region.byte_count,
            )
            .is_none()
            {
                return Err(PhysicalMemoryError::AddressOverflow);
            }
            if matches!(
                reservation.source,
                ReservationSource::DeviceTree { no_map: true }
            ) && regions_overlap(reservation.region, bootstrap_mapped_ram)
            {
                // The strict kernel direct map does not yet omit these mappings.
                return Err(PhysicalMemoryError::UnsupportedNoMapReservation);
            }
        }
        let bitmap_storage_bytes = align_up(bitmap_storage_byte_count, PHYSICAL_FRAME_BYTE_COUNT)
            .ok_or(PhysicalMemoryError::AddressOverflow)?;
        if bitmap_storage_bytes == 0 {
            return Err(PhysicalMemoryError::BitmapStorageUnavailable);
        }

        let (mut protected_regions, mut protected_region_count) =
            collect_protected_regions(reservations, managed_bank, kernel_end, device_tree_region)?;
        // Checked slicing ensures a bad count cannot make reservations disappear.
        let protected = protected_regions
            .get(..protected_region_count)
            .ok_or(PhysicalMemoryError::TooManyReservations)?;
        let bitmap_storage =
            find_bitmap_storage(managed_bank, kernel_end, bitmap_storage_bytes, protected)?;
        let bitmap_destination = protected_regions
            .get_mut(protected_region_count)
            .ok_or(PhysicalMemoryError::TooManyReservations)?;
        *bitmap_destination = bitmap_storage;
        protected_region_count += 1;

        Ok(Self {
            managed_bank,
            bitmap_storage,
            protected_regions,
            protected_region_count,
        })
    }

    /// Returns the sole RAM bank from which the early frame allocator may allocate.
    #[must_use]
    pub const fn managed_bank(&self) -> PhysicalRegion {
        self.managed_bank
    }

    /// Returns the page-aligned physical region where the bitmap must be initialized.
    #[must_use]
    pub const fn bitmap_storage(&self) -> PhysicalRegion {
        self.bitmap_storage
    }

    /// Returns the list of memory ranges excluded from frame allocation and release.
    #[must_use]
    pub fn protected_regions(&self) -> &[PhysicalRegion] {
        // A corrupted count cannot silently hide memory from the protection list.
        self.protected_regions
            .get(..self.protected_region_count)
            .unwrap_or(&self.protected_regions)
    }

    /// Checks whether a full frame intersects protected memory.
    #[must_use]
    pub fn is_protected_frame(&self, frame_address: u64) -> bool {
        let Some(frame) = PhysicalRegion::new(frame_address, PHYSICAL_FRAME_BYTE_COUNT) else {
            return true;
        };
        self.protected_regions()
            .iter()
            .any(|region| regions_overlap(*region, frame))
    }

    /// Returns a conservative, page-rounded bitmap interval for a protected region.
    ///
    /// The interval is relative to the managed bank; a region wholly outside that bank
    /// produces `None`. Partial pages are included, not silently left allocatable.
    #[must_use]
    pub fn protected_frame_interval(
        &self,
        protected_region: PhysicalRegion,
    ) -> Option<(usize, usize)> {
        let first_byte = protected_region
            .start_address
            .max(self.managed_bank.start_address);
        let last_byte = protected_region
            .end_address()
            .min(self.managed_bank.end_address());
        if first_byte >= last_byte {
            return None;
        }
        let first_index =
            (first_byte - self.managed_bank.start_address) / PHYSICAL_FRAME_BYTE_COUNT;
        let final_index =
            (last_byte - self.managed_bank.start_address).div_ceil(PHYSICAL_FRAME_BYTE_COUNT);
        Some((
            usize::try_from(first_index).ok()?,
            usize::try_from(final_index - first_index).ok()?,
        ))
    }
}

#[cfg(test)]
mod tests {
    //! Host-side checks for multibank discovery, collision avoidance, and fail-closed behavior.

    use super::*;

    /// Creates a checked RAM or reserved physical extent.
    fn region(start: u64, len: u64) -> PhysicalRegion {
        PhysicalRegion::new(start, len).unwrap()
    }

    /// Creates a firmware reservation for testing.
    fn reserved(start: u64, len: u64) -> ReservedRegion {
        ReservedRegion {
            region: region(start, len),
            source: ReservationSource::Firmware,
        }
    }

    /// Returns the RAM interval present in QEMU's initial translation tables.
    fn mapped_ram() -> PhysicalRegion {
        region(0x4000_0000, 0x8000_0000)
    }

    /// Plans an allocator across one 512 MiB boot-mapped bank.
    fn plan(
        reservations: &[ReservedRegion],
        dtb: PhysicalRegion,
    ) -> Result<PhysicalMemoryPlan, PhysicalMemoryError> {
        let banks = [
            region(0x4000_0000, 0x2000_0000),
            region(0x1_0000_0000, 0x2000_0000),
        ];
        PhysicalMemoryPlan::new(
            banks[0],
            mapped_ram(),
            &banks,
            reservations,
            0,
            0x4024_0000,
            dtb,
            8192,
        )
    }

    /// Firmware reservations and firmware DTBs are never allocatable, including partial pages.
    #[test]
    fn reserves_firmware_and_device_tree_pages() {
        let p = plan(&[reserved(0x4100_0001, 2)], region(0x4000_0000, 1200)).unwrap();
        assert!(p.is_protected_frame(0x4100_0000));
        assert!(p.is_protected_frame(0x4000_0000));
        assert!(p.is_protected_frame(0x4023_f000));
        assert!(!p.is_protected_frame(0x4110_0000));
        assert_eq!(
            p.protected_frame_interval(region(0x4100_0001, 2)),
            Some((0x1000, 1))
        );
    }

    /// Bitmap storage moves past reservations instead of zeroing them.
    #[test]
    fn bitmap_skips_reserved_or_dtb_memory() {
        let p = plan(
            &[reserved(0x4024_0000, 0x4000)],
            region(0x4028_0000, 0x1000),
        )
        .unwrap();
        assert_eq!(p.bitmap_storage().start_address, 0x4024_4000); // after the firmware reservation
    }

    /// A second RAM bank is recognized but not handed to the unmapped bootstrap allocator.
    #[test]
    fn additional_banks_are_validated_but_not_allocated() {
        let p = plan(&[], region(0x4000_0000, 0x1000)).unwrap();
        assert_eq!(p.managed_bank(), region(0x4000_0000, 0x2000_0000));
        assert_eq!(
            p.protected_frame_interval(region(0x1_0000_0000, 0x1000)),
            None
        );
    }

    /// The actual frame bitmap skips every partially reserved physical page.
    #[test]
    fn bitmap_never_allocates_protected_frames() {
        use crate::frame_bitmap::FrameBitmap;

        let p = plan(
            &[reserved(0x4024_5001, 2), reserved(0x4100_0000, 8192)],
            region(0x4000_0000, 1200),
        )
        .unwrap();
        let mut words = [0_u64; 2048];
        let mut bitmap = FrameBitmap::new(&mut words, 131_072).unwrap();
        for &protected in p.protected_regions() {
            if let Some((first_index, count)) = p.protected_frame_interval(protected) {
                bitmap.mark_used_range(first_index, count);
            }
        }
        for _ in 0..1500 {
            let index = bitmap.allocate().unwrap();
            let frame_address =
                p.managed_bank().start_address + (index as u64) * PHYSICAL_FRAME_BYTE_COUNT;
            assert!(!p.is_protected_frame(frame_address));
        }
    }

    /// Unresolved allocations are refused rather than racing firmware ownership.
    #[test]
    fn rejects_dynamic_reservations() {
        let bank = region(0x4000_0000, 0x2000_0000);
        assert_eq!(
            PhysicalMemoryPlan::new(
                bank,
                mapped_ram(),
                &[bank],
                &[],
                1,
                0x4024_0000,
                region(0x4000_0000, 0x1000),
                8192
            ),
            Err(PhysicalMemoryError::UnresolvedDynamicReservation)
        );
    }

    /// `no-map` memory cannot be included in an ordinary RAM direct-map.
    #[test]
    fn rejects_no_map_reservations() {
        let bank = region(0x4000_0000, 0x2000_0000);
        let reservation = ReservedRegion {
            region: region(0x4500_0000, 0x1000),
            source: ReservationSource::DeviceTree { no_map: true },
        };
        assert_eq!(
            PhysicalMemoryPlan::new(
                bank,
                mapped_ram(),
                &[bank],
                &[reservation],
                0,
                0x4024_0000,
                region(0x4000_0000, 0x1000),
                8192
            ),
            Err(PhysicalMemoryError::UnsupportedNoMapReservation)
        );
    }

    /// A bank outside the early page tables cannot be used as bitmap storage.
    #[test]
    fn rejects_unmapped_bootstrap_ram() {
        let bank = region(0x1_0000_0000, 0x2000_0000);
        assert_eq!(
            PhysicalMemoryPlan::new(
                bank,
                mapped_ram(),
                &[bank],
                &[],
                0,
                0x1_0024_0000,
                region(0x4000_0000, 0x1000),
                8192
            ),
            Err(PhysicalMemoryError::OutsideBootstrapMapping)
        );
    }

    /// RAM banks must never overlap even when only the first one is currently mapped.
    #[test]
    fn rejects_overlapping_banks() {
        let first = region(0x4000_0000, 0x2000_0000);
        let second = region(0x5000_0000, 0x2000_0000);
        assert_eq!(
            PhysicalMemoryPlan::new(
                first,
                mapped_ram(),
                &[first, second],
                &[],
                0,
                0x4024_0000,
                region(0x4000_0000, 0x1000),
                8192
            ),
            Err(PhysicalMemoryError::OverlappingMemoryBanks)
        );
    }

    /// Gaps must fit the full bitmap or initialization refuses to touch memory.
    #[test]
    fn rejects_bitmap_exhaustion() {
        let bank = region(0x4000_0000, 0x2000_0000);
        assert_eq!(
            PhysicalMemoryPlan::new(
                bank,
                mapped_ram(),
                &[bank],
                &[],
                0,
                0x5fff_f000,
                region(0x4000_0000, 0x1000),
                8192
            ),
            Err(PhysicalMemoryError::BitmapStorageUnavailable)
        );
    }
}
