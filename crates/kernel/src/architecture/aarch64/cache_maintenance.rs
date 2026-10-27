//! Keeping the instruction cache in step with code the kernel has just written.

const DATA_CACHE_LINE_SHIFT_FIELD: u64 = 16;
const DATA_CACHE_LINE_SHIFT_MASK: u64 = 0xf;
/// The field holds log2 of the line size in 4-byte words.
const WORD_BYTE_COUNT: usize = 4;

fn data_cache_line_byte_count() -> usize {
    let cache_type: u64;
    // SAFETY: Reading `CTR_EL0` has no side effects.
    unsafe {
        core::arch::asm!(
            "mrs {cache_type}, ctr_el0",
            cache_type = out(reg) cache_type,
            options(nomem, nostack, preserves_flags),
        );
    }
    let line_shift = (cache_type >> DATA_CACHE_LINE_SHIFT_FIELD) & DATA_CACHE_LINE_SHIFT_MASK;
    WORD_BYTE_COUNT << line_shift
}

/// Makes the `byte_count` bytes at the kernel address `start_address` visible to instruction fetch.
///
/// Call after writing code through the data side and before any core may execute it: the data
/// is pushed to the point of unification and stale instructions are dropped.
pub fn make_code_visible(start_address: usize, byte_count: usize) {
    let line_byte_count = data_cache_line_byte_count();
    let end_address = start_address + byte_count;
    let mut line_address = start_address & !(line_byte_count - 1);
    while line_address < end_address {
        // SAFETY: Cleaning a data cache line by address only writes it back to memory.
        unsafe {
            core::arch::asm!(
                "dc cvau, {line_address}",
                line_address = in(reg) line_address,
                options(nostack, preserves_flags),
            );
        }
        line_address += line_byte_count;
    }
    // SAFETY: Invalidating the instruction cache only forces instructions to be fetched again.
    unsafe {
        core::arch::asm!("dsb ish", "ic ialluis", "dsb ish", "isb", options(nostack),);
    }
}
