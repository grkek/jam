//! Switches the calling core to the kernel's strict translation tables.
//!
//! The boot code in `boot_entry.s` already turned translation on with coarse tables.

/// `TCR_EL1.EPD0`: stop walking `TTBR0_EL1`, so the lower half maps nothing.
const TCR_LOWER_HALF_WALKS_DISABLED: u64 = 1 << 7;

/// Points `TTBR1_EL1` at `root_table_address` and turns off lower-half translation on this core.
///
/// The tables must map the running code, the stack, and the UART exactly as the boot tables do,
/// because execution continues at the same addresses. Called by `enable_kernel_address_space` and
/// by every secondary core once it reaches Rust.
pub(crate) fn switch_kernel_translation_root(root_table_address: usize) {
    // SAFETY: The caller guarantees the new tables keep the running code, stack, and UART mapped.
    // The barriers make the table writes visible to the walker first, then flush stale
    // translations before anything runs on the new tables.
    unsafe {
        core::arch::asm!(
            "dsb ish",
            "msr ttbr1_el1, {root_table_address}",
            "mrs {scratch}, tcr_el1",
            "orr {scratch}, {scratch}, {walks_disabled}",
            "msr tcr_el1, {scratch}",
            "isb",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            root_table_address = in(reg) root_table_address as u64,
            walks_disabled = in(reg) TCR_LOWER_HALF_WALKS_DISABLED,
            scratch = out(reg) _,
            options(nostack),
        );
    }
}
