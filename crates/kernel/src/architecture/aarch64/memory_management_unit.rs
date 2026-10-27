//! Switches translation tables on the calling core.
//!
//! The boot code in `boot_entry.s` already turned translation on with coarse tables. The upper
//! half belongs to the kernel and is the same everywhere. The lower half belongs to whichever
//! actor runs, and each actor has its own address space identifier, so switching between actors
//! needs no TLB flush.

/// `TCR_EL1.EPD0`: stop walking `TTBR0_EL1`, so the lower half maps nothing.
const TCR_LOWER_HALF_WALKS_DISABLED: u64 = 1 << 7;
/// Where `TTBR0_EL1` keeps the address space identifier.
const ADDRESS_SPACE_IDENTIFIER_SHIFT: u64 = 48;

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

/// Makes the actor's address space the lower half on this core.
///
/// `root_table_address` is the physical address of the actor's root table.
pub(crate) fn activate_lower_half(root_table_address: usize, address_space_identifier: u16) {
    let translation_base = (root_table_address as u64)
        | (u64::from(address_space_identifier) << ADDRESS_SPACE_IDENTIFIER_SHIFT);
    // SAFETY: The tables belong to a live actor and were completely written before the actor was
    // queued. The first barrier orders those writes before the walker can read them. Only the
    // lower half changes, and the kernel's own code and data live in the upper half.
    unsafe {
        core::arch::asm!(
            "dsb ish",
            "msr ttbr0_el1, {translation_base}",
            "mrs {scratch}, tcr_el1",
            "bic {scratch}, {scratch}, {walks_disabled}",
            "msr tcr_el1, {scratch}",
            "isb",
            translation_base = in(reg) translation_base,
            walks_disabled = in(reg) TCR_LOWER_HALF_WALKS_DISABLED,
            scratch = out(reg) _,
            options(nostack),
        );
    }
}

/// Stops using any actor's lower half on this core, for when it has nothing to run.
pub(crate) fn deactivate_lower_half() {
    // SAFETY: Setting `EPD0` only stops lower-half walks. The kernel runs from the upper half.
    unsafe {
        core::arch::asm!(
            "mrs {scratch}, tcr_el1",
            "orr {scratch}, {scratch}, {walks_disabled}",
            "msr tcr_el1, {scratch}",
            "isb",
            walks_disabled = in(reg) TCR_LOWER_HALF_WALKS_DISABLED,
            scratch = out(reg) _,
            options(nostack),
        );
    }
}

/// Discards every cached translation tagged with `address_space_identifier`, on all cores.
///
/// Needed when an identifier is handed to a new address space.
pub(crate) fn invalidate_address_space_identifier(address_space_identifier: u16) {
    let operand = u64::from(address_space_identifier) << ADDRESS_SPACE_IDENTIFIER_SHIFT;
    // SAFETY: Invalidating cached translations never breaks anything, it only forces new walks.
    unsafe {
        core::arch::asm!(
            "tlbi aside1is, {operand}",
            "dsb ish",
            "isb",
            operand = in(reg) operand,
            options(nostack),
        );
    }
}
