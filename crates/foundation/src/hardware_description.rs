//! Discovers physical RAM, reserved memory, and CPU affinity IDs from a flattened device tree.
//!
//! This is a bounded, allocation-free discovery pass for early AArch64 boot. The existing
//! `device_tree` module remains the source of the legacy QEMU boot summary until the kernel
//! memory manager and CPU bootstrap can consume the full hardware description safely.
//! All integers read from the flattened device tree are big-endian.

/// Maximum number of RAM extents retained during early discovery.
pub const MAXIMUM_MEMORY_REGIONS: usize = 16;
/// Maximum number of firmware and device-tree reservations retained during early discovery.
pub const MAXIMUM_RESERVED_REGIONS: usize = 32;
/// Maximum number of CPU affinity identifiers retained during early discovery.
pub const MAXIMUM_CPU_IDENTIFIERS: usize = 8;

/// Magic word at the beginning of a flattened device tree.
const DEVICE_TREE_MAGIC: u32 = 0xd00d_feed;
/// Minimum number of bytes in the version-17 flattened-device-tree header.
const HEADER_BYTE_COUNT: usize = 40;
/// Minimum supported structure format version; this version supplies structure sizes.
const MINIMUM_FORMAT_VERSION: u32 = 17;
/// Size of an FDT property or structure token.
const WORD_BYTE_COUNT: usize = 4;
/// Size of a pair of big-endian firmware reservation values.
const RESERVATION_ENTRY_BYTE_COUNT: usize = 16;
/// The root node's default number of address cells.
const DEFAULT_ROOT_ADDRESS_CELLS: usize = 2;
/// The root node's default number of size cells, as defined by the specification.
const DEFAULT_ROOT_SIZE_CELLS: usize = 1;
/// Default CPU-address cell count if `/cpus` does not override it.
const DEFAULT_CPU_ADDRESS_CELLS: usize = 1;
/// FDT begin-node token.
const BEGIN_NODE: u32 = 1;
/// FDT end-node token.
const END_NODE: u32 = 2;
/// FDT property token.
const PROPERTY: u32 = 3;
/// FDT no-operation token.
const NOP: u32 = 4;
/// FDT end-of-structure token.
const END: u32 = 9;

/// A physical address range, expressed without assuming that RAM is contiguous.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhysicalRegion {
    /// First physical byte in the region.
    pub start_address: u64,
    /// Number of bytes beginning at `start_address`.
    pub byte_count: u64,
}

impl PhysicalRegion {
    /// Builds a nonempty region whose end is representable as `u64`.
    #[must_use]
    pub const fn new(start_address: u64, byte_count: u64) -> Option<Self> {
        if byte_count == 0 || start_address.checked_add(byte_count).is_none() {
            return None;
        }
        Some(Self {
            start_address,
            byte_count,
        })
    }

    /// Exclusive end address of this checked region.
    #[must_use]
    pub const fn end_address(self) -> u64 {
        self.start_address + self.byte_count
    }
}

/// Describes where a reserved region came from and how it may be mapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReservationSource {
    /// Firmware reservation-table entry, not available for general allocation.
    Firmware,
    /// A memory region reserved by the device tree.
    DeviceTree {
        /// Whether the device-tree node forbids mapping this region.
        no_map: bool,
    },
}

/// A physical reservation that must be excluded from ordinary frame allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservedRegion {
    /// Physical extent reserved by firmware or a device-tree node.
    pub region: PhysicalRegion,
    /// Reservation provenance and mapping restrictions.
    pub source: ReservationSource,
}

/// A fixed-capacity hardware inventory discovered from the firmware device tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HardwareDescription {
    memory_regions: [PhysicalRegion; MAXIMUM_MEMORY_REGIONS],
    memory_region_count: usize,
    reserved_regions: [ReservedRegion; MAXIMUM_RESERVED_REGIONS],
    reserved_region_count: usize,
    cpu_identifiers: [u64; MAXIMUM_CPU_IDENTIFIERS],
    cpu_identifier_count: usize,
    dynamic_reservation_count: usize,
}

impl HardwareDescription {
    /// Returns physical RAM extents without treating gaps between them as usable memory.
    #[must_use]
    pub fn memory_regions(&self) -> &[PhysicalRegion] {
        // An impossible count must not make unknown memory allocatable.
        self.memory_regions
            .get(..self.memory_region_count)
            .unwrap_or(&[])
    }

    /// Returns firmware and statically reserved physical memory extents.
    #[must_use]
    pub fn reserved_regions(&self) -> &[ReservedRegion] {
        // On corrupted bookkeeping, retain every stored reservation rather than dropping them.
        self.reserved_regions
            .get(..self.reserved_region_count)
            .unwrap_or(&self.reserved_regions)
    }

    /// Returns the CPU hardware identifiers encoded in `/cpus/cpu@*/reg`.
    #[must_use]
    pub fn cpu_identifiers(&self) -> &[u64] {
        // Unknown topology must never start an unverified core.
        self.cpu_identifiers
            .get(..self.cpu_identifier_count)
            .unwrap_or(&[])
    }

    /// Counts dynamic reservation requests that cannot yet be placed by JAM's allocator.
    #[must_use]
    pub const fn dynamic_reservation_count(&self) -> usize {
        self.dynamic_reservation_count
    }

    /// Appends a validated RAM extent without silently truncating the inventory.
    fn push_memory(&mut self, region: PhysicalRegion) -> Result<(), DiscoveryError> {
        let destination = self
            .memory_regions
            .get_mut(self.memory_region_count)
            .ok_or(DiscoveryError::TooManyMemoryRegions)?;
        *destination = region;
        self.memory_region_count += 1;
        Ok(())
    }

    /// Appends a reservation without silently truncating the inventory.
    fn push_reserved(&mut self, region: ReservedRegion) -> Result<(), DiscoveryError> {
        let destination = self
            .reserved_regions
            .get_mut(self.reserved_region_count)
            .ok_or(DiscoveryError::TooManyReservedRegions)?;
        *destination = region;
        self.reserved_region_count += 1;
        Ok(())
    }

    /// Appends a CPU identifier, rejecting duplicate identities and excess CPUs.
    fn push_cpu(&mut self, identifier: u64) -> Result<(), DiscoveryError> {
        if self.cpu_identifiers().contains(&identifier) {
            return Err(DiscoveryError::DuplicateCpuIdentifier);
        }
        let destination = self
            .cpu_identifiers
            .get_mut(self.cpu_identifier_count)
            .ok_or(DiscoveryError::TooManyCpuIdentifiers)?;
        *destination = identifier;
        self.cpu_identifier_count += 1;
        Ok(())
    }
}

/// Why hardware discovery could not safely interpret a firmware device tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoveryError {
    /// Missing or incorrect device-tree magic.
    BadMagic,
    /// Header, structure, strings, or reservation entry extends beyond the blob.
    Truncated,
    /// Structure tokens, offsets, or property formats are inconsistent.
    Malformed,
    /// Format predates the header fields used by this parser.
    UnsupportedVersion,
    /// Region is empty or its exclusive end overflows the address space.
    InvalidRegion,
    /// No RAM extent was described by the device tree.
    MissingMemory,
    /// Device tree declares more RAM extents than the fixed capacity.
    TooManyMemoryRegions,
    /// Device tree declares more reserved extents than the fixed capacity.
    TooManyReservedRegions,
    /// Device tree declares more CPUs than this early inventory can represent.
    TooManyCpuIdentifiers,
    /// Multiple CPU nodes describe the same hardware identifier.
    DuplicateCpuIdentifier,
    /// Reserved-memory addresses require unsupported bus-address translation.
    UnsupportedReservationTranslation,
}

/// Reads one big-endian 32-bit field at a checked offset.
fn read_word(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(WORD_BYTE_COUNT)?;
    let value: [u8; WORD_BYTE_COUNT] = bytes.get(offset..end)?.try_into().ok()?;
    Some(u32::from_be_bytes(value))
}

/// Reads one big-endian 64-bit field at a checked offset.
fn read_double_word(bytes: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    let value: [u8; 8] = bytes.get(offset..end)?.try_into().ok()?;
    Some(u64::from_be_bytes(value))
}

/// Parses a byte length or FDT offset from a 32-bit header field.
fn header_offset(blob: &[u8], offset: usize) -> Result<usize, DiscoveryError> {
    usize::try_from(read_word(blob, offset).ok_or(DiscoveryError::Truncated)?)
        .map_err(|_| DiscoveryError::Malformed)
}

/// Obtains a bounded section without overflowing offset arithmetic.
fn section(bytes: &[u8], start: usize, byte_count: usize) -> Result<&[u8], DiscoveryError> {
    let end = start
        .checked_add(byte_count)
        .ok_or(DiscoveryError::Malformed)?;
    bytes.get(start..end).ok_or(DiscoveryError::Truncated)
}

/// Reads a zero-terminated name from the structure or string table.
fn zero_terminated(bytes: &[u8], offset: usize) -> Result<&[u8], DiscoveryError> {
    let suffix = bytes.get(offset..).ok_or(DiscoveryError::Malformed)?;
    let length = suffix
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(DiscoveryError::Truncated)?;
    suffix.get(..length).ok_or(DiscoveryError::Malformed)
}

/// Aligns the next FDT token to four bytes, checking for integer overflow.
fn align_token(offset: usize) -> Result<usize, DiscoveryError> {
    offset
        .checked_add(WORD_BYTE_COUNT - 1)
        .map(|value| value & !(WORD_BYTE_COUNT - 1))
        .ok_or(DiscoveryError::Malformed)
}

/// Reads one address or size encoded in one or two big-endian FDT cells.
fn read_cells(bytes: &[u8], cell_count: usize) -> Result<u64, DiscoveryError> {
    if !(1..=2).contains(&cell_count) || bytes.len() != cell_count * WORD_BYTE_COUNT {
        return Err(DiscoveryError::Malformed);
    }
    let mut value = 0_u64;
    for offset in (0..bytes.len()).step_by(WORD_BYTE_COUNT) {
        value =
            (value << 32) | u64::from(read_word(bytes, offset).ok_or(DiscoveryError::Truncated)?);
    }
    Ok(value)
}

/// Reads a nonzero count of cells from an address/size-cells property.
fn cell_count(data: &[u8]) -> Result<usize, DiscoveryError> {
    if data.len() != WORD_BYTE_COUNT {
        return Err(DiscoveryError::Malformed);
    }
    let count = usize::try_from(read_word(data, 0).ok_or(DiscoveryError::Truncated)?)
        .map_err(|_| DiscoveryError::Malformed)?;
    if !(1..=2).contains(&count) {
        return Err(DiscoveryError::Malformed);
    }
    Ok(count)
}

/// Walks each address/size pair in a `reg` property without allocating a collection.
fn visit_regions(
    data: &[u8],
    address_cells: usize,
    size_cells: usize,
    mut operation: impl FnMut(PhysicalRegion) -> Result<(), DiscoveryError>,
) -> Result<(), DiscoveryError> {
    let address_bytes = address_cells * WORD_BYTE_COUNT;
    let size_bytes = size_cells * WORD_BYTE_COUNT;
    let stride = address_bytes + size_bytes;
    if stride == 0 || data.is_empty() || data.len() % stride != 0 {
        return Err(DiscoveryError::Malformed);
    }
    for pair in data.chunks_exact(stride) {
        let address = pair.get(..address_bytes).ok_or(DiscoveryError::Malformed)?;
        let size = pair.get(address_bytes..).ok_or(DiscoveryError::Malformed)?;
        let start_address = read_cells(address, address_cells)?;
        let byte_count = read_cells(size, size_cells)?;
        let region =
            PhysicalRegion::new(start_address, byte_count).ok_or(DiscoveryError::InvalidRegion)?;
        operation(region)?;
    }
    Ok(())
}

/// Returns `true` for `memory` and `memory@...` node names only.
fn memory_node(name: &[u8]) -> bool {
    name == b"memory" || name.starts_with(b"memory@")
}

/// Returns `true` for `cpu` and `cpu@...` node names only.
fn cpu_node(name: &[u8]) -> bool {
    name == b"cpu" || name.starts_with(b"cpu@")
}

/// Validated slices of an FDT and the locations needed for firmware reservations.
struct FdtSections<'blob> {
    /// Entire firmware-provided blob, bounded by the header's total size.
    blob: &'blob [u8],
    /// Structure tokens describing nodes and properties.
    structure: &'blob [u8],
    /// Names of properties referenced by the structure block.
    strings: &'blob [u8],
    /// First byte of the firmware memory-reservation table.
    reservation_offset: usize,
    /// First byte of the structure block, beyond the reservation table.
    structure_offset: usize,
}

/// Validates the FDT header before returning any section slices.
fn read_fdt_sections(blob: &[u8]) -> Result<FdtSections<'_>, DiscoveryError> {
    if read_word(blob, 0) != Some(DEVICE_TREE_MAGIC) {
        return Err(DiscoveryError::BadMagic);
    }
    if blob.len() < HEADER_BYTE_COUNT {
        return Err(DiscoveryError::Truncated);
    }
    let total_byte_count = header_offset(blob, 4)?;
    if total_byte_count < HEADER_BYTE_COUNT {
        return Err(DiscoveryError::Malformed);
    }
    let blob = section(blob, 0, total_byte_count)?;
    let structure_offset = header_offset(blob, 8)?;
    let string_offset = header_offset(blob, 12)?;
    let reservation_offset = header_offset(blob, 16)?;
    if read_word(blob, 20).ok_or(DiscoveryError::Truncated)? < MINIMUM_FORMAT_VERSION {
        return Err(DiscoveryError::UnsupportedVersion);
    }
    let string_byte_count = header_offset(blob, 32)?;
    let structure_byte_count = header_offset(blob, 36)?;
    Ok(FdtSections {
        blob,
        structure: section(blob, structure_offset, structure_byte_count)?,
        strings: section(blob, string_offset, string_byte_count)?,
        reservation_offset,
        structure_offset,
    })
}

/// Creates an empty fixed-capacity inventory without allocating or touching hardware.
fn empty_hardware_description() -> HardwareDescription {
    let empty_reservation = ReservedRegion {
        region: PhysicalRegion {
            start_address: 0,
            byte_count: 0,
        },
        source: ReservationSource::Firmware,
    };
    HardwareDescription {
        memory_regions: [PhysicalRegion::default(); MAXIMUM_MEMORY_REGIONS],
        memory_region_count: 0,
        reserved_regions: [empty_reservation; MAXIMUM_RESERVED_REGIONS],
        reserved_region_count: 0,
        cpu_identifiers: [0; MAXIMUM_CPU_IDENTIFIERS],
        cpu_identifier_count: 0,
        dynamic_reservation_count: 0,
    }
}

/// Reads all firmware reservations before any node-specific reservations are appended.
fn read_firmware_reservations(
    sections: &FdtSections<'_>,
    description: &mut HardwareDescription,
) -> Result<(), DiscoveryError> {
    if sections.reservation_offset % 8 != 0
        || sections.reservation_offset < HEADER_BYTE_COUNT
        || sections.reservation_offset >= sections.structure_offset
    {
        return Err(DiscoveryError::Malformed);
    }
    let mut cursor = sections.reservation_offset;
    loop {
        let next_cursor = cursor
            .checked_add(RESERVATION_ENTRY_BYTE_COUNT)
            .ok_or(DiscoveryError::Malformed)?;
        if next_cursor > sections.structure_offset {
            return Err(DiscoveryError::Malformed);
        }
        let entry = section(sections.blob, cursor, RESERVATION_ENTRY_BYTE_COUNT)?;
        let start_address = read_double_word(entry, 0).ok_or(DiscoveryError::Truncated)?;
        let byte_count = read_double_word(entry, 8).ok_or(DiscoveryError::Truncated)?;
        cursor = next_cursor;
        if start_address == 0 && byte_count == 0 {
            return Ok(());
        }
        description.push_reserved(ReservedRegion {
            region: PhysicalRegion::new(start_address, byte_count)
                .ok_or(DiscoveryError::InvalidRegion)?,
            source: ReservationSource::Firmware,
        })?;
    }
}

/// Kind of top-level `DeviceTree` node currently being visited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ParentNodeKind {
    #[default]
    Other,
    Memory,
    CentralProcessingUnits,
    ReservedMemory,
}

/// Kind of child node currently being visited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ChildNodeKind {
    #[default]
    Other,
    CentralProcessingUnit,
    Reservation,
}

/// Properties collected from the current reserved-memory child.
#[derive(Clone, Copy, Debug, Default)]
struct ReservationAttributes {
    /// Whether the child declares a static physical region.
    has_register_property: bool,
    /// Whether the child requests dynamic memory allocation.
    has_size_property: bool,
    /// Whether the child prohibits ordinary memory mappings.
    no_map: bool,
}

/// Mutable parser state for the root, CPU, RAM, and reserved-memory node levels.
///
/// Only the first three levels matter. Deeper subtrees are traversed but ignored,
/// so a nested peripheral cannot accidentally inherit the parent's reservation flags.
struct StructureWalker {
    /// Inventory accumulated from firmware records and structure properties.
    description: HardwareDescription,
    /// Address-cell width declared by the root node.
    root_address_cells: usize,
    /// Size-cell width declared by the root node.
    root_size_cells: usize,
    /// Address-cell width for the `/cpus` node.
    cpu_address_cells: usize,
    /// Address-cell width for `/reserved-memory` children.
    reserved_address_cells: usize,
    /// Size-cell width for `/reserved-memory` children.
    reserved_size_cells: usize,
    /// Current FDT node nesting depth.
    depth: usize,
    /// Hardware affinity identifier of the current CPU node.
    cpu_identifier: Option<u64>,
    /// Whether the current CPU node has status `disabled`.
    cpu_disabled: bool,
    /// First reservation inserted by the current reserved-memory child.
    reservation_start_index: usize,
    /// Current top-level node category.
    parent_node_kind: ParentNodeKind,
    /// Current child node category.
    child_node_kind: ChildNodeKind,
    /// Properties of the current reserved-memory child.
    reservation_attributes: ReservationAttributes,
}

impl StructureWalker {
    /// Initializes node tracking with the specification's default cell sizes.
    fn new(description: &HardwareDescription) -> Self {
        Self {
            description: *description,
            root_address_cells: DEFAULT_ROOT_ADDRESS_CELLS,
            root_size_cells: DEFAULT_ROOT_SIZE_CELLS,
            cpu_address_cells: DEFAULT_CPU_ADDRESS_CELLS,
            reserved_address_cells: DEFAULT_ROOT_ADDRESS_CELLS,
            reserved_size_cells: DEFAULT_ROOT_SIZE_CELLS,
            depth: 0,
            parent_node_kind: ParentNodeKind::Other,
            child_node_kind: ChildNodeKind::Other,
            reservation_attributes: ReservationAttributes::default(),
            cpu_identifier: None,
            cpu_disabled: false,
            reservation_start_index: 0,
        }
    }

    /// Opens a node, resetting state only at the relevant tree depth.
    fn begin_node(&mut self, name: &[u8]) -> Result<(), DiscoveryError> {
        self.depth = self.depth.checked_add(1).ok_or(DiscoveryError::Malformed)?;

        if self.depth == 2 {
            self.parent_node_kind = if memory_node(name) {
                ParentNodeKind::Memory
            } else if name == b"cpus" {
                ParentNodeKind::CentralProcessingUnits
            } else if name == b"reserved-memory" {
                ParentNodeKind::ReservedMemory
            } else {
                ParentNodeKind::Other
            };

            if self.parent_node_kind == ParentNodeKind::ReservedMemory {
                self.reserved_address_cells = self.root_address_cells;
                self.reserved_size_cells = self.root_size_cells;
            }
        } else if self.depth == 3 {
            self.child_node_kind = match self.parent_node_kind {
                ParentNodeKind::CentralProcessingUnits if cpu_node(name) => {
                    ChildNodeKind::CentralProcessingUnit
                }
                ParentNodeKind::ReservedMemory => ChildNodeKind::Reservation,
                _ => ChildNodeKind::Other,
            };

            self.cpu_identifier = None;
            self.cpu_disabled = false;
            self.reservation_start_index = self.description.reserved_region_count;
            self.reservation_attributes = ReservationAttributes::default();
        }

        Ok(())
    }

    /// Closes a node and applies reservation metadata to *only that node's* entries.
    fn end_node(&mut self) -> Result<(), DiscoveryError> {
        if self.depth == 0 {
            return Err(DiscoveryError::Malformed);
        }

        if self.depth == 3 {
            if self.child_node_kind == ChildNodeKind::CentralProcessingUnit && !self.cpu_disabled {
                self.description
                    .push_cpu(self.cpu_identifier.ok_or(DiscoveryError::Malformed)?)?;
            }

            if self.child_node_kind == ChildNodeKind::Reservation {
                if self.reservation_attributes.has_size_property
                    && !self.reservation_attributes.has_register_property
                {
                    self.description.dynamic_reservation_count = self
                        .description
                        .dynamic_reservation_count
                        .checked_add(1)
                        .ok_or(DiscoveryError::Malformed)?;
                }

                // The start index was captured when this reservation child opened.
                // Checked slicing prevents malformed indices from modifying unrelated entries.
                let reservations = self
                    .description
                    .reserved_regions
                    .get_mut(self.reservation_start_index..self.description.reserved_region_count)
                    .ok_or(DiscoveryError::Malformed)?;

                for reservation in reservations {
                    reservation.source = ReservationSource::DeviceTree {
                        no_map: self.reservation_attributes.no_map,
                    };
                }
            }

            self.child_node_kind = ChildNodeKind::Other;
        } else if self.depth == 2 {
            self.parent_node_kind = ParentNodeKind::Other;
        }

        self.depth -= 1;

        Ok(())
    }

    /// Handles properties of RAM, CPUs, and reserved-memory children.
    fn property(&mut self, name: &[u8], data: &[u8]) -> Result<(), DiscoveryError> {
        match self.depth {
            1 => {
                if name == b"#address-cells" {
                    self.root_address_cells = cell_count(data)?;
                } else if name == b"#size-cells" {
                    self.root_size_cells = cell_count(data)?;
                }
            }
            2 => match self.parent_node_kind {
                ParentNodeKind::Memory if name == b"reg" => {
                    visit_regions(
                        data,
                        self.root_address_cells,
                        self.root_size_cells,
                        |region| self.description.push_memory(region),
                    )?;
                }
                ParentNodeKind::CentralProcessingUnits if name == b"#address-cells" => {
                    self.cpu_address_cells = cell_count(data)?;
                }
                ParentNodeKind::ReservedMemory => {
                    if name == b"#address-cells" {
                        self.reserved_address_cells = cell_count(data)?;
                    } else if name == b"#size-cells" {
                        self.reserved_size_cells = cell_count(data)?;
                    } else if name == b"ranges" && !data.is_empty() {
                        return Err(DiscoveryError::UnsupportedReservationTranslation);
                    }
                }
                _ => {}
            },
            3 => {
                self.child_property(name, data)?;
            }
            _ => {}
        }

        Ok(())
    }

    /// Handles a CPU or static/dynamic reserved-memory child property.
    fn child_property(&mut self, name: &[u8], data: &[u8]) -> Result<(), DiscoveryError> {
        match self.child_node_kind {
            ChildNodeKind::CentralProcessingUnit => {
                if name == b"reg" {
                    self.cpu_identifier = Some(read_cells(data, self.cpu_address_cells)?);
                } else if name == b"status" {
                    self.cpu_disabled = data.starts_with(b"disabled\0");
                }
            }
            ChildNodeKind::Reservation => {
                if name == b"reg" {
                    visit_regions(
                        data,
                        self.reserved_address_cells,
                        self.reserved_size_cells,
                        |region| {
                            self.description.push_reserved(ReservedRegion {
                                region,
                                source: ReservationSource::DeviceTree { no_map: false },
                            })
                        },
                    )?;

                    self.reservation_attributes.has_register_property = true;
                } else if name == b"size" {
                    self.reservation_attributes.has_size_property = true;
                } else if name == b"no-map" {
                    self.reservation_attributes.no_map = true;
                }
            }
            ChildNodeKind::Other => {}
        }

        Ok(())
    }

    /// Walks the structure block, rejecting unterminated and unbalanced node streams.
    fn walk(
        mut self,
        structure: &[u8],
        strings: &[u8],
    ) -> Result<HardwareDescription, DiscoveryError> {
        let mut offset = 0_usize;
        let mut ended = false;
        while offset < structure.len() {
            let token = read_word(structure, offset).ok_or(DiscoveryError::Truncated)?;
            offset = offset
                .checked_add(WORD_BYTE_COUNT)
                .ok_or(DiscoveryError::Malformed)?;
            match token {
                BEGIN_NODE => {
                    let name = zero_terminated(structure, offset)?;
                    offset = align_token(
                        offset
                            .checked_add(name.len() + 1)
                            .ok_or(DiscoveryError::Malformed)?,
                    )?;
                    self.begin_node(name)?;
                }
                END_NODE => self.end_node()?,
                PROPERTY => {
                    let byte_count = header_offset(structure, offset)?;
                    let name_offset = header_offset(
                        structure,
                        offset
                            .checked_add(WORD_BYTE_COUNT)
                            .ok_or(DiscoveryError::Malformed)?,
                    )?;
                    offset = offset
                        .checked_add(2 * WORD_BYTE_COUNT)
                        .ok_or(DiscoveryError::Malformed)?;
                    let data = section(structure, offset, byte_count)?;
                    offset = align_token(
                        offset
                            .checked_add(byte_count)
                            .ok_or(DiscoveryError::Malformed)?,
                    )?;
                    let name = zero_terminated(strings, name_offset)?;
                    self.property(name, data)?;
                }
                NOP => {}
                END => {
                    if self.depth != 0 {
                        return Err(DiscoveryError::Malformed);
                    }
                    ended = true;
                    break;
                }
                _ => return Err(DiscoveryError::Malformed),
            }
        }
        if !ended {
            return Err(DiscoveryError::Truncated);
        }
        if self.description.memory_region_count == 0 {
            return Err(DiscoveryError::MissingMemory);
        }
        Ok(self.description)
    }
}

/// Discovers bounded memory and CPU topology metadata from a version-17-or-newer FDT blob.
///
/// The description does not install mappings or begin scheduling. Callers must
/// validate discovered memory ownership before giving any frames to actors.
///
/// # Errors
///
/// Rejects malformed blobs, unsupported reservation translations, invalid extents,
/// and inventories that exceed their fixed capacities.
pub fn discover_hardware(blob: &[u8]) -> Result<HardwareDescription, DiscoveryError> {
    let sections = read_fdt_sections(blob)?;
    let mut description = empty_hardware_description();
    read_firmware_reservations(&sections, &mut description)?;
    StructureWalker::new(&description).walk(sections.structure, sections.strings)
}

#[cfg(test)]
mod tests {
    //! Validates multi-bank RAM, mixed reservations, CPU affinity, and malformed firmware data.

    use super::*;
    use std::vec::Vec;

    /// Builds a small, spec-shaped version-17 FDT for host-side tests.
    struct TestTree {
        structure: Vec<u8>,
        strings: Vec<u8>,
        reservations: Vec<(u64, u64)>,
    }

    impl TestTree {
        /// Creates an empty FDT body with no reservations.
        fn new() -> Self {
            Self {
                structure: Vec::new(),
                strings: Vec::new(),
                reservations: Vec::new(),
            }
        }

        /// Appends a big-endian token or property word.
        fn word(&mut self, value: u32) {
            self.structure.extend_from_slice(&value.to_be_bytes());
        }

        /// Pads the structure block to its four-byte alignment.
        fn align(&mut self) {
            while self.structure.len() % 4 != 0 {
                self.structure.push(0);
            }
        }

        /// Begins a named node.
        fn begin(&mut self, name: &str) {
            self.word(BEGIN_NODE);
            self.structure.extend_from_slice(name.as_bytes());
            self.structure.push(0);
            self.align();
        }

        /// Ends the current node.
        fn end(&mut self) {
            self.word(END_NODE);
        }

        /// Writes a property into the string and structure blocks.
        fn property(&mut self, name: &str, data: &[u8]) {
            let name_offset = self.strings.len();
            self.strings.extend_from_slice(name.as_bytes());
            self.strings.push(0);
            self.word(PROPERTY);
            self.word(u32::try_from(data.len()).unwrap());
            self.word(u32::try_from(name_offset).unwrap());
            self.structure.extend_from_slice(data);
            self.align();
        }

        /// Produces a complete device-tree blob with a terminated reservation table.
        fn build(mut self) -> Vec<u8> {
            self.word(END);
            let reservation_offset =
                u32::try_from(HEADER_BYTE_COUNT).expect("Header size exceeds u32");
            let reservation_byte_count = (self.reservations.len() + 1) * 16;
            let structure_offset = reservation_offset
                + u32::try_from(reservation_byte_count).expect("Reservation size exceeds u32");
            let string_offset = structure_offset + u32::try_from(self.structure.len()).unwrap();
            let total_size = string_offset + u32::try_from(self.strings.len()).unwrap();
            let fields = [
                DEVICE_TREE_MAGIC,
                total_size,
                structure_offset,
                string_offset,
                reservation_offset,
                17,
                16,
                0,
                u32::try_from(self.strings.len()).unwrap(),
                u32::try_from(self.structure.len()).unwrap(),
            ];
            let mut blob: Vec<u8> = fields
                .iter()
                .flat_map(|value| value.to_be_bytes())
                .collect();
            for (address, size) in self.reservations {
                blob.extend_from_slice(&address.to_be_bytes());
                blob.extend_from_slice(&size.to_be_bytes());
            }
            blob.extend_from_slice(&[0; 16]);
            blob.extend_from_slice(&self.structure);
            blob.extend_from_slice(&self.strings);
            blob
        }
    }

    /// Turns a slice of 32-bit cells into big-endian FDT property bytes.
    fn cells(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_be_bytes()).collect()
    }

    /// Returns a tree with two RAM banks, one static reservation and two CPUs.
    fn sample() -> Vec<u8> {
        let mut tree = TestTree::new();
        tree.reservations.push((0x4000_2000, 0x1000));
        tree.begin("");
        tree.property("#address-cells", &cells(&[2]));
        tree.property("#size-cells", &cells(&[2]));
        tree.begin("memory@40000000");
        tree.property(
            "reg",
            &cells(&[
                0,
                0x4000_0000,
                0,
                0x1000_0000,
                1,
                0x0000_0000,
                0,
                0x2000_0000,
            ]),
        );
        tree.end();
        tree.begin("reserved-memory");
        tree.property("#address-cells", &cells(&[2]));
        tree.property("#size-cells", &cells(&[2]));
        tree.property("ranges", &[]);
        tree.begin("framebuffer@48000000");
        tree.property("reg", &cells(&[0, 0x4800_0000, 0, 0x8000]));
        tree.property("no-map", &[]);
        tree.end();
        tree.begin("pool");
        tree.property("size", &cells(&[0, 0x1000_0000]));
        tree.end();
        tree.end();
        tree.begin("cpus");
        tree.property("#address-cells", &cells(&[2]));
        tree.property("#size-cells", &cells(&[0]));
        tree.begin("cpu@0");
        tree.property("reg", &cells(&[0, 0]));
        tree.end();
        tree.begin("cpu@100");
        tree.property("reg", &cells(&[0, 0x100]));
        tree.end();
        tree.begin("cpu@200");
        tree.property("reg", &cells(&[0, 0x200]));
        tree.property("status", b"disabled\0");
        tree.end();
        tree.end();
        tree.end();
        tree.build()
    }

    /// Confirms the parser preserves disjoint RAM and CPU hardware identifiers.
    #[test]
    fn discovers_ram_reservations_and_cpu_identifiers() {
        let inventory = discover_hardware(&sample()).unwrap();
        assert_eq!(
            inventory.memory_regions(),
            &[
                PhysicalRegion::new(0x4000_0000, 0x1000_0000).unwrap(),
                PhysicalRegion::new(0x1_0000_0000, 0x2000_0000).unwrap(),
            ]
        );
        assert_eq!(inventory.cpu_identifiers(), &[0, 0x100]);
        assert_eq!(inventory.reserved_regions().len(), 2);
        assert_eq!(
            inventory.reserved_regions()[0].source,
            ReservationSource::Firmware
        );
        assert_eq!(
            inventory.reserved_regions()[1].source,
            ReservationSource::DeviceTree { no_map: true }
        );
        assert_eq!(inventory.dynamic_reservation_count(), 1);
    }

    /// Static reservations retain their own `no-map` state without corrupting firmware entries.
    #[test]
    fn static_reservations_keep_independent_sources() {
        let mut tree = TestTree::new();
        tree.reservations.push((0x4000_2000, 0x1000));
        tree.begin("");
        tree.property("#address-cells", &cells(&[1]));
        tree.property("#size-cells", &cells(&[1]));
        tree.begin("memory");
        tree.property("reg", &cells(&[0x4000_0000, 0x2000_0000]));
        tree.end();
        tree.begin("reserved-memory");
        tree.property("#address-cells", &cells(&[1]));
        tree.property("#size-cells", &cells(&[1]));
        tree.property("ranges", &[]);
        tree.begin("protected@41000000");
        tree.property("reg", &cells(&[0x4100_0000, 0x1000]));
        tree.property("no-map", &[]);
        tree.end();
        tree.begin("ordinary@42000000");
        tree.property("reg", &cells(&[0x4200_0000, 0x1000]));
        tree.end();
        tree.end();
        tree.end();

        let description = discover_hardware(&tree.build()).unwrap();
        assert_eq!(description.reserved_regions().len(), 3);
        let sources: std::vec::Vec<_> = description
            .reserved_regions()
            .iter()
            .map(|reservation| reservation.source)
            .collect();
        assert_eq!(
            sources,
            [
                ReservationSource::Firmware,
                ReservationSource::DeviceTree { no_map: true },
                ReservationSource::DeviceTree { no_map: false },
            ]
        );
    }

    /// Confirms that truncated buffers cannot trigger out-of-bounds accesses.
    #[test]
    fn rejects_truncated_blob() {
        let blob = sample();
        assert_eq!(
            discover_hardware(blob.get(..25).unwrap_or(&[])),
            Err(DiscoveryError::Truncated)
        );
    }

    /// Confirms incorrect firmware magic is rejected.
    #[test]
    fn rejects_bad_magic() {
        let mut blob = sample();
        blob[0] = 0;
        assert_eq!(discover_hardware(&blob), Err(DiscoveryError::BadMagic));
    }

    /// Rejects duplicate hardware affinities rather than starting a CPU twice.
    #[test]
    fn rejects_duplicate_cpu_affinity() {
        let mut tree = TestTree::new();
        tree.begin("");
        tree.property("#address-cells", &cells(&[1]));
        tree.property("#size-cells", &cells(&[1]));
        tree.begin("memory");
        tree.property("reg", &cells(&[0x4000_0000, 0x1000_0000]));
        tree.end();
        tree.begin("cpus");
        tree.begin("cpu@0");
        tree.property("reg", &cells(&[0]));
        tree.end();
        tree.begin("cpu@1");
        tree.property("reg", &cells(&[0]));
        tree.end();
        tree.end();
        tree.end();
        assert_eq!(
            discover_hardware(&tree.build()),
            Err(DiscoveryError::DuplicateCpuIdentifier)
        );
    }

    /// Rejects a reserved-memory bus mapping that requires address translation.
    #[test]
    fn rejects_nonidentity_reserved_memory_ranges() {
        let mut tree = TestTree::new();
        tree.begin("");
        tree.begin("memory");
        tree.property("reg", &cells(&[0, 0x4000_0000, 0x1000_0000]));
        tree.end();
        tree.begin("reserved-memory");
        tree.property("ranges", &cells(&[0, 0, 0, 0]));
        tree.end();
        tree.end();
        assert_eq!(
            discover_hardware(&tree.build()),
            Err(DiscoveryError::UnsupportedReservationTranslation)
        );
    }

    /// Confirms that overflow in physical address arithmetic is rejected.
    #[test]
    fn rejects_overflowing_memory_region() {
        let mut tree = TestTree::new();
        tree.begin("");
        tree.property("#address-cells", &cells(&[2]));
        tree.property("#size-cells", &cells(&[2]));
        tree.begin("memory");
        tree.property("reg", &cells(&[0xffff_ffff, 0xffff_f000, 0, 0x2000]));
        tree.end();
        tree.end();
        assert_eq!(
            discover_hardware(&tree.build()),
            Err(DiscoveryError::InvalidRegion)
        );
    }
}
