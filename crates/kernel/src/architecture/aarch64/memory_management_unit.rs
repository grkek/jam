//! Programs the translation registers and turns the memory management unit on.

use foundation::page_table_entry::{DEVICE_MEMORY_ATTRIBUTE_INDEX, NORMAL_MEMORY_ATTRIBUTE_INDEX};

/// `MAIR_EL1` attribute: device memory, non-gathering, non-reordering, no early write acknowledgement.
const DEVICE_MEMORY_ATTRIBUTES: u64 = 0x00;
/// `MAIR_EL1` attribute: normal memory, write-back, read- and write-allocate.
const NORMAL_MEMORY_ATTRIBUTES: u64 = 0xff;
const BITS_PER_MEMORY_ATTRIBUTE: u64 = 8;

/// 39-bit virtual addresses, so translation starts at level one.
const TRANSLATION_SIZE_OFFSET: u64 = 25;
const TCR_INNER_WRITE_BACK_CACHEABLE: u64 = 0b01 << 8;
const TCR_OUTER_WRITE_BACK_CACHEABLE: u64 = 0b01 << 10;
const TCR_INNER_SHAREABLE: u64 = 0b11 << 12;
const TCR_GRANULE_4_KIB: u64 = 0b00 << 14;
const TCR_UPPER_SIZE_OFFSET_SHIFT: u64 = 16;
/// The upper half is unused for now, so its translation walks are disabled.
const TCR_UPPER_WALKS_DISABLED: u64 = 1 << 23;
const TCR_UPPER_GRANULE_4_KIB: u64 = 0b10 << 30;
const TCR_PHYSICAL_ADDRESS_SIZE_SHIFT: u64 = 32;
const PHYSICAL_ADDRESS_RANGE_MASK: u64 = 0xf;
/// 48 bits of physical address, the most the page table format here describes.
const PHYSICAL_ADDRESS_RANGE_LIMIT: u64 = 0b0101;

const SCTLR_MMU_ENABLE: u64 = 1 << 0;
const SCTLR_DATA_CACHE_ENABLE: u64 = 1 << 2;
const SCTLR_INSTRUCTION_CACHE_ENABLE: u64 = 1 << 12;
/// Writable memory is never executable, enforced by the hardware as well.
const SCTLR_WRITE_IMPLIES_EXECUTE_NEVER: u64 = 1 << 19;

/// Switches the memory management unit and caches on with the tables at `root_table_address`.
///
/// The tables must map the code that is running, the stack, and the UART, because execution
/// continues at the same addresses. Only `enable_kernel_address_space` calls this.
pub(crate) fn switch_on_memory_management_unit(root_table_address: usize) {
    let memory_attributes = (DEVICE_MEMORY_ATTRIBUTES
        << (DEVICE_MEMORY_ATTRIBUTE_INDEX * BITS_PER_MEMORY_ATTRIBUTE))
        | (NORMAL_MEMORY_ATTRIBUTES << (NORMAL_MEMORY_ATTRIBUTE_INDEX * BITS_PER_MEMORY_ATTRIBUTE));
    let enable_bits = SCTLR_MMU_ENABLE
        | SCTLR_DATA_CACHE_ENABLE
        | SCTLR_INSTRUCTION_CACHE_ENABLE
        | SCTLR_WRITE_IMPLIES_EXECUTE_NEVER;

    // SAFETY: Reading an ID register has no side effects.
    let physical_address_range = unsafe {
        let register_value: u64;
        core::arch::asm!(
            "mrs {register_value}, id_aa64mmfr0_el1",
            register_value = out(reg) register_value,
            options(nomem, nostack, preserves_flags),
        );
        register_value & PHYSICAL_ADDRESS_RANGE_MASK
    }
    .min(PHYSICAL_ADDRESS_RANGE_LIMIT);

    let translation_control = TRANSLATION_SIZE_OFFSET
        | TCR_INNER_WRITE_BACK_CACHEABLE
        | TCR_OUTER_WRITE_BACK_CACHEABLE
        | TCR_INNER_SHAREABLE
        | TCR_GRANULE_4_KIB
        | (TRANSLATION_SIZE_OFFSET << TCR_UPPER_SIZE_OFFSET_SHIFT)
        | TCR_UPPER_WALKS_DISABLED
        | TCR_UPPER_GRANULE_4_KIB
        | (physical_address_range << TCR_PHYSICAL_ADDRESS_SIZE_SHIFT);

    // SAFETY: The caller guarantees the tables identity-map the running code, the stack, and the
    // UART, so execution continues unchanged when translation turns on. The barriers order the
    // table writes before the walker can read them and flush stale translations and instructions.
    unsafe {
        core::arch::asm!(
            "dsb ish",
            "msr mair_el1, {memory_attributes}",
            "msr tcr_el1, {translation_control}",
            "msr ttbr0_el1, {root_table_address}",
            "isb",
            "tlbi vmalle1",
            "ic iallu",
            "dsb ish",
            "isb",
            "mrs {scratch}, sctlr_el1",
            "orr {scratch}, {scratch}, {enable_bits}",
            "msr sctlr_el1, {scratch}",
            "isb",
            memory_attributes = in(reg) memory_attributes,
            translation_control = in(reg) translation_control,
            root_table_address = in(reg) root_table_address as u64,
            enable_bits = in(reg) enable_bits,
            scratch = out(reg) _,
            options(nostack),
        );
    }
}
