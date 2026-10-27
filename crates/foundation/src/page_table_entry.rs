//! Encoding of `AArch64` translation table entries for the 4 KiB granule.
//!
//! Permissions are enforced when an entry is built: nothing is ever both writable and
//! executable, device memory is never executable, and the kernel never executes user memory.

/// Bytes in a page.
pub const PAGE_BYTE_COUNT: usize = 4096;
/// Bytes in a 2 MiB block.
pub const BLOCK_BYTE_COUNT: usize = 2 * 1024 * 1024;
/// Entries in one translation table.
pub const ENTRIES_PER_TABLE: usize = 512;
/// `MAIR_EL1` attribute index that selects device memory.
pub const DEVICE_MEMORY_ATTRIBUTE_INDEX: u64 = 0;
/// `MAIR_EL1` attribute index that selects normal, cacheable memory.
pub const NORMAL_MEMORY_ATTRIBUTE_INDEX: u64 = 1;

const OUTPUT_ADDRESS_LIMIT: u64 = 1 << 48;
const OUTPUT_ADDRESS_MASK: u64 = 0x0000_ffff_ffff_f000;

const VALID_BIT: u64 = 1 << 0;
const TABLE_OR_PAGE_BIT: u64 = 1 << 1;
const ATTRIBUTE_INDEX_SHIFT: u64 = 2;
/// `AP[1]`: code running at EL0 may access the page.
const USER_ACCESS_BIT: u64 = 1 << 6;
/// `AP[2]`: the page is read-only.
const READ_ONLY_BIT: u64 = 1 << 7;
const SHAREABILITY_SHIFT: u64 = 8;
const INNER_SHAREABLE: u64 = 0b11;
const OUTER_SHAREABLE: u64 = 0b10;
const ACCESS_FLAG_BIT: u64 = 1 << 10;
/// Cached translations are tagged with the address space identifier instead of shared by all.
const NON_GLOBAL_BIT: u64 = 1 << 11;
const PRIVILEGED_EXECUTE_NEVER_BIT: u64 = 1 << 53;
const UNPRIVILEGED_EXECUTE_NEVER_BIT: u64 = 1 << 54;

/// What kind of memory a mapping describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    /// Ordinary RAM: cacheable and shareable.
    Normal,
    /// Memory-mapped devices: uncached, with strict ordering.
    Device,
}

/// What the kernel may do with a mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelAccess {
    /// The kernel may read and write.
    ReadWrite,
    /// The kernel may only read.
    ReadOnly,
}

/// What code running at EL0 may do with a mapping.
///
/// Reading is always included, because the hardware cannot grant EL0 write or execute
/// without read. The kernel's own access follows: it may write exactly what EL0 may write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserAccess {
    /// EL0 may read.
    ReadOnly,
    /// EL0 may read and write.
    ReadWrite,
}

/// Why a combination of attributes was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageAttributeError {
    /// Kernel memory that can be written must never be executable.
    WritableAndExecutable,
    /// Device memory must never be executable.
    DeviceExecutable,
    /// User memory that can be written must never be executable.
    UserWritableAndExecutable,
}

/// The permissions and memory type of a mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageAttributes {
    memory_kind: MemoryKind,
    kernel_access: KernelAccess,
    kernel_executable: bool,
    user_access: Option<UserAccess>,
    user_executable: bool,
    non_global: bool,
}

impl PageAttributes {
    /// Builds attributes for a mapping only the kernel can use, refusing combinations that
    /// break write-xor-execute.
    ///
    /// # Errors
    ///
    /// Fails when the mapping would be both writable and executable, or executable device memory.
    pub fn new(
        memory_kind: MemoryKind,
        kernel_access: KernelAccess,
        kernel_executable: bool,
    ) -> Result<Self, PageAttributeError> {
        if kernel_executable && kernel_access == KernelAccess::ReadWrite {
            return Err(PageAttributeError::WritableAndExecutable);
        }
        if kernel_executable && memory_kind == MemoryKind::Device {
            return Err(PageAttributeError::DeviceExecutable);
        }
        Ok(Self {
            memory_kind,
            kernel_access,
            kernel_executable,
            user_access: None,
            user_executable: false,
            non_global: false,
        })
    }

    /// Builds attributes for ordinary memory that code at EL0 may use.
    ///
    /// The kernel never executes user memory. Executable user memory is read-only, and the
    /// mapping is tagged with the address space identifier.
    ///
    /// # Errors
    ///
    /// Fails when the mapping would be both writable and executable.
    pub fn user(access: UserAccess, executable: bool) -> Result<Self, PageAttributeError> {
        if executable && access == UserAccess::ReadWrite {
            return Err(PageAttributeError::UserWritableAndExecutable);
        }
        let kernel_access = match access {
            UserAccess::ReadWrite => KernelAccess::ReadWrite,
            UserAccess::ReadOnly => KernelAccess::ReadOnly,
        };
        Ok(Self {
            memory_kind: MemoryKind::Normal,
            kernel_access,
            kernel_executable: false,
            user_access: Some(access),
            user_executable: executable,
            non_global: true,
        })
    }

    /// Tags the mapping with the address space identifier, as every lower-half mapping must be.
    /// Without it, cached translations would be shared between actors.
    #[must_use]
    pub const fn per_address_space(self) -> Self {
        Self {
            non_global: true,
            ..self
        }
    }

    fn bits(self) -> u64 {
        let (attribute_index, shareability) = match self.memory_kind {
            MemoryKind::Normal => (NORMAL_MEMORY_ATTRIBUTE_INDEX, INNER_SHAREABLE),
            MemoryKind::Device => (DEVICE_MEMORY_ATTRIBUTE_INDEX, OUTER_SHAREABLE),
        };
        let mut bits = ACCESS_FLAG_BIT
            | (attribute_index << ATTRIBUTE_INDEX_SHIFT)
            | (shareability << SHAREABILITY_SHIFT);

        let read_only = match self.user_access {
            Some(UserAccess::ReadOnly) => true,
            Some(UserAccess::ReadWrite) => false,
            None => self.kernel_access == KernelAccess::ReadOnly,
        };
        if read_only {
            bits |= READ_ONLY_BIT;
        }
        if self.user_access.is_some() {
            bits |= USER_ACCESS_BIT;
        }
        if !self.user_executable {
            bits |= UNPRIVILEGED_EXECUTE_NEVER_BIT;
        }
        if !self.kernel_executable {
            bits |= PRIVILEGED_EXECUTE_NEVER_BIT;
        }
        if self.non_global {
            bits |= NON_GLOBAL_BIT;
        }
        bits
    }
}

/// One entry of a translation table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageTableEntry(u64);

fn is_valid_output_address(address: u64, alignment: usize) -> bool {
    address % (alignment as u64) == 0 && address < OUTPUT_ADDRESS_LIMIT
}

impl PageTableEntry {
    /// An entry that maps nothing.
    pub const INVALID: Self = Self(0);

    /// Reinterprets a raw word read from a translation table.
    #[must_use]
    pub const fn from_raw_value(raw_value: u64) -> Self {
        Self(raw_value)
    }

    /// The raw word to store in a translation table.
    #[must_use]
    pub const fn raw_value(self) -> u64 {
        self.0
    }

    /// Whether the entry maps or points to anything.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.0 & VALID_BIT != 0
    }

    /// Whether the entry points to a next-level table (or, at the last level, maps a page).
    #[must_use]
    pub const fn is_table_or_page(self) -> bool {
        self.0 & TABLE_OR_PAGE_BIT != 0
    }

    /// The physical address the entry points to.
    #[must_use]
    pub const fn output_address(self) -> u64 {
        self.0 & OUTPUT_ADDRESS_MASK
    }

    /// Whether code at EL0 may access the mapped page.
    #[must_use]
    pub const fn is_user_accessible(self) -> bool {
        self.0 & USER_ACCESS_BIT != 0
    }

    /// Whether the mapped page is read-only.
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        self.0 & READ_ONLY_BIT != 0
    }

    /// Whether code at EL0 may execute the mapped page.
    #[must_use]
    pub const fn is_user_executable(self) -> bool {
        self.0 & UNPRIVILEGED_EXECUTE_NEVER_BIT == 0
    }

    /// Whether the kernel may execute the mapped page.
    #[must_use]
    pub const fn is_privileged_executable(self) -> bool {
        self.0 & PRIVILEGED_EXECUTE_NEVER_BIT == 0
    }

    /// Whether cached translations of the page are tagged with an address space identifier.
    #[must_use]
    pub const fn is_non_global(self) -> bool {
        self.0 & NON_GLOBAL_BIT != 0
    }

    /// An entry that points to the next-level table at `next_table_address`.
    #[must_use]
    pub fn table(next_table_address: u64) -> Option<Self> {
        if !is_valid_output_address(next_table_address, PAGE_BYTE_COUNT) {
            return None;
        }
        Some(Self(VALID_BIT | TABLE_OR_PAGE_BIT | next_table_address))
    }

    /// A last-level entry that maps one 4 KiB page.
    #[must_use]
    pub fn page(physical_address: u64, attributes: PageAttributes) -> Option<Self> {
        if !is_valid_output_address(physical_address, PAGE_BYTE_COUNT) {
            return None;
        }
        Some(Self(
            VALID_BIT | TABLE_OR_PAGE_BIT | physical_address | attributes.bits(),
        ))
    }

    /// A level-two entry that maps one 2 MiB block.
    #[must_use]
    pub fn block(physical_address: u64, attributes: PageAttributes) -> Option<Self> {
        if !is_valid_output_address(physical_address, BLOCK_BYTE_COUNT) {
            return None;
        }
        Some(Self(VALID_BIT | physical_address | attributes.bits()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attributes(kind: MemoryKind, access: KernelAccess, executable: bool) -> PageAttributes {
        PageAttributes::new(kind, access, executable).unwrap()
    }

    fn user_page(access: UserAccess, executable: bool) -> PageTableEntry {
        let attributes = PageAttributes::user(access, executable).unwrap();
        PageTableEntry::page(0x4030_0000, attributes).unwrap()
    }

    #[test]
    fn writable_and_executable_is_refused() {
        assert_eq!(
            PageAttributes::new(MemoryKind::Normal, KernelAccess::ReadWrite, true),
            Err(PageAttributeError::WritableAndExecutable)
        );
    }

    #[test]
    fn executable_device_memory_is_refused() {
        assert_eq!(
            PageAttributes::new(MemoryKind::Device, KernelAccess::ReadOnly, true),
            Err(PageAttributeError::DeviceExecutable)
        );
    }

    #[test]
    fn code_pages_are_read_only_and_executable() {
        let entry = PageTableEntry::page(
            0x4020_0000,
            attributes(MemoryKind::Normal, KernelAccess::ReadOnly, true),
        )
        .unwrap();
        assert_eq!(entry.raw_value() & 0b11, 0b11);
        assert!(entry.is_read_only());
        assert!(entry.is_privileged_executable());
        assert!(!entry.is_user_executable());
        assert!(!entry.is_user_accessible());
        assert_eq!(entry.output_address(), 0x4020_0000);
    }

    #[test]
    fn data_pages_are_writable_and_never_executable() {
        let entry = PageTableEntry::page(
            0x4020_1000,
            attributes(MemoryKind::Normal, KernelAccess::ReadWrite, false),
        )
        .unwrap();
        assert!(!entry.is_read_only());
        assert!(!entry.is_privileged_executable());
        assert!(!entry.is_user_executable());
        assert_ne!(entry.raw_value() & ACCESS_FLAG_BIT, 0);
    }

    #[test]
    fn kernel_pages_are_global_and_invisible_to_user_code() {
        let entry = PageTableEntry::page(
            0x4020_1000,
            attributes(MemoryKind::Normal, KernelAccess::ReadWrite, false),
        )
        .unwrap();
        assert!(!entry.is_non_global());
        assert!(!entry.is_user_accessible());
    }

    #[test]
    fn device_pages_use_the_device_attribute() {
        let entry = PageTableEntry::block(
            0x0900_0000,
            attributes(MemoryKind::Device, KernelAccess::ReadWrite, false),
        )
        .unwrap();
        assert_eq!(
            (entry.raw_value() >> ATTRIBUTE_INDEX_SHIFT) & 0b111,
            DEVICE_MEMORY_ATTRIBUTE_INDEX
        );
    }

    #[test]
    fn a_block_is_not_a_table_and_a_table_is() {
        let block = PageTableEntry::block(
            0x4000_0000,
            attributes(MemoryKind::Normal, KernelAccess::ReadWrite, false),
        )
        .unwrap();
        let table = PageTableEntry::table(0x4030_0000).unwrap();
        assert!(block.is_valid() && !block.is_table_or_page());
        assert!(table.is_valid() && table.is_table_or_page());
        assert_eq!(table.output_address(), 0x4030_0000);
    }

    #[test]
    fn misaligned_addresses_are_refused() {
        let normal = attributes(MemoryKind::Normal, KernelAccess::ReadWrite, false);
        assert!(PageTableEntry::page(0x4020_0001, normal).is_none());
        assert!(PageTableEntry::block(0x4020_1000, normal).is_none());
        assert!(PageTableEntry::table(0x4020_0800).is_none());
    }

    #[test]
    fn invalid_entry_maps_nothing() {
        assert!(!PageTableEntry::INVALID.is_valid());
    }

    #[test]
    fn user_data_is_writable_and_never_executable_by_anyone() {
        let entry = user_page(UserAccess::ReadWrite, false);
        assert!(entry.is_user_accessible());
        assert!(!entry.is_read_only());
        assert!(!entry.is_user_executable());
        assert!(!entry.is_privileged_executable());
        assert!(entry.is_non_global());
    }

    #[test]
    fn user_constants_are_read_only_and_never_executable() {
        let entry = user_page(UserAccess::ReadOnly, false);
        assert!(entry.is_user_accessible());
        assert!(entry.is_read_only());
        assert!(!entry.is_user_executable());
        assert!(!entry.is_privileged_executable());
    }

    #[test]
    fn user_code_is_read_only_and_runs_only_at_el0() {
        let entry = user_page(UserAccess::ReadOnly, true);
        assert!(entry.is_user_accessible());
        assert!(entry.is_read_only());
        assert!(entry.is_user_executable());
        assert!(!entry.is_privileged_executable());
        assert!(entry.is_non_global());
    }

    #[test]
    fn writable_user_code_is_refused() {
        assert_eq!(
            PageAttributes::user(UserAccess::ReadWrite, true),
            Err(PageAttributeError::UserWritableAndExecutable)
        );
    }

    #[test]
    fn per_address_space_only_adds_the_non_global_tag() {
        let global = attributes(MemoryKind::Normal, KernelAccess::ReadWrite, false);
        let tagged = global.per_address_space();
        let global_entry = PageTableEntry::page(0x4030_0000, global).unwrap();
        let tagged_entry = PageTableEntry::page(0x4030_0000, tagged).unwrap();
        assert!(!global_entry.is_non_global());
        assert!(tagged_entry.is_non_global());
        assert_eq!(
            global_entry.raw_value() | NON_GLOBAL_BIT,
            tagged_entry.raw_value()
        );
    }
}
