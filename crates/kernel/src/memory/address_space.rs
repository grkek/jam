//! A set of translation tables describing what an execution context may touch.

use foundation::page_table_entry::{
    BLOCK_BYTE_COUNT, PAGE_BYTE_COUNT, PageAttributes, PageTableEntry,
};

use super::{frame_allocator::FrameAllocator, page_tables::TableFrame};

const LEVEL_ONE_SHIFT: usize = 30;
const LEVEL_TWO_SHIFT: usize = 21;
const LEVEL_THREE_SHIFT: usize = 12;
const TABLE_INDEX_MASK: usize = 0x1ff;
/// Virtual addresses are 39 bits wide, so translation starts at level one.
const VIRTUAL_ADDRESS_LIMIT: usize = 1 << 39;

/// Why a mapping could not be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// No free frame was left for a new translation table.
    OutOfFrames,
    /// An address is misaligned for the size of mapping requested.
    Misaligned,
    /// The virtual address is outside the address space.
    InvalidAddress,
    /// Something is already mapped there.
    AlreadyMapped,
}

/// The translation tables of one address space.
pub struct AddressSpace {
    root_table: TableFrame,
}

fn table_index(virtual_address: usize, shift: usize) -> usize {
    (virtual_address >> shift) & TABLE_INDEX_MASK
}

impl AddressSpace {
    /// Creates an address space that maps nothing.
    pub fn new(frame_allocator: &mut FrameAllocator) -> Option<Self> {
        Some(Self {
            root_table: TableFrame::allocate(frame_allocator)?,
        })
    }

    /// Physical address of the root table, as `TTBR0_EL1` expects it.
    #[must_use]
    pub const fn root_table_address(&self) -> usize {
        self.root_table.physical_address()
    }

    /// Follows the entry at `index`, creating the next-level table when there is none.
    fn descend(
        table: TableFrame,
        index: usize,
        frame_allocator: &mut FrameAllocator,
    ) -> Result<TableFrame, MapError> {
        let entry = table.read(index).ok_or(MapError::InvalidAddress)?;
        if entry.is_valid() {
            if !entry.is_table_or_page() {
                return Err(MapError::AlreadyMapped);
            }
            let next_address =
                usize::try_from(entry.output_address()).map_err(|_| MapError::InvalidAddress)?;
            return Ok(TableFrame::from_physical_address(next_address));
        }
        let next_table = TableFrame::allocate(frame_allocator).ok_or(MapError::OutOfFrames)?;
        let entry = PageTableEntry::table(next_table.physical_address() as u64)
            .ok_or(MapError::Misaligned)?;
        table.write(index, entry).ok_or(MapError::InvalidAddress)?;
        Ok(next_table)
    }

    fn install(table: TableFrame, index: usize, entry: PageTableEntry) -> Result<(), MapError> {
        let existing = table.read(index).ok_or(MapError::InvalidAddress)?;
        if existing.is_valid() {
            return Err(MapError::AlreadyMapped);
        }
        table.write(index, entry).ok_or(MapError::InvalidAddress)
    }

    /// Maps one 2 MiB block.
    ///
    /// # Errors
    ///
    /// Fails on misaligned or out-of-range addresses, when something is already mapped there,
    /// or when no frame is left for a table.
    pub fn map_block(
        &mut self,
        frame_allocator: &mut FrameAllocator,
        virtual_address: usize,
        physical_address: usize,
        attributes: PageAttributes,
    ) -> Result<(), MapError> {
        if virtual_address >= VIRTUAL_ADDRESS_LIMIT {
            return Err(MapError::InvalidAddress);
        }
        if virtual_address % BLOCK_BYTE_COUNT != 0 {
            return Err(MapError::Misaligned);
        }
        let level_two = Self::descend(
            self.root_table,
            table_index(virtual_address, LEVEL_ONE_SHIFT),
            frame_allocator,
        )?;
        let entry = PageTableEntry::block(physical_address as u64, attributes)
            .ok_or(MapError::Misaligned)?;
        Self::install(
            level_two,
            table_index(virtual_address, LEVEL_TWO_SHIFT),
            entry,
        )
    }

    /// Maps one 4 KiB page.
    ///
    /// # Errors
    ///
    /// Fails on misaligned or out-of-range addresses, when something is already mapped there,
    /// or when no frame is left for a table.
    pub fn map_page(
        &mut self,
        frame_allocator: &mut FrameAllocator,
        virtual_address: usize,
        physical_address: usize,
        attributes: PageAttributes,
    ) -> Result<(), MapError> {
        if virtual_address >= VIRTUAL_ADDRESS_LIMIT {
            return Err(MapError::InvalidAddress);
        }
        if virtual_address % PAGE_BYTE_COUNT != 0 {
            return Err(MapError::Misaligned);
        }
        let level_two = Self::descend(
            self.root_table,
            table_index(virtual_address, LEVEL_ONE_SHIFT),
            frame_allocator,
        )?;
        let level_three = Self::descend(
            level_two,
            table_index(virtual_address, LEVEL_TWO_SHIFT),
            frame_allocator,
        )?;
        let entry = PageTableEntry::page(physical_address as u64, attributes)
            .ok_or(MapError::Misaligned)?;
        Self::install(
            level_three,
            table_index(virtual_address, LEVEL_THREE_SHIFT),
            entry,
        )
    }
}
