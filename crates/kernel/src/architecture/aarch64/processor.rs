//! Instructions and registers that belong to the executing core.

/// Sleeps until an event arrives or an interrupt is taken.
///
/// Returns at once when an event is already pending.
pub fn wait_for_event() {
    // SAFETY: `wfe` only pauses the core until the next event.
    unsafe {
        core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
    }
}

/// Sets the event flag of every core, waking those sleeping in `wait_for_event`.
pub fn send_event() {
    // SAFETY: `sev` only signals an event.
    unsafe {
        core::arch::asm!("sev", options(nomem, nostack, preserves_flags));
    }
}

/// The core's number as the hardware reports it: the lowest affinity field of `MPIDR_EL1`.
///
/// On QEMU `virt` this is the core's index. Real hardware needs a mapping from the device tree.
#[must_use]
pub fn read_hardware_core_number() -> usize {
    let multiprocessor_affinity: u64;
    // SAFETY: Reading `MPIDR_EL1` has no side effects.
    unsafe {
        core::arch::asm!(
            "mrs {multiprocessor_affinity}, mpidr_el1",
            multiprocessor_affinity = out(reg) multiprocessor_affinity,
            options(nomem, nostack, preserves_flags),
        );
    }
    (multiprocessor_affinity & 0xff) as usize
}

/// Reads the kernel's index for this core, kept in `TPIDR_EL1`.
#[must_use]
pub fn read_current_core_index() -> Result<usize, core::num::TryFromIntError> {
    let core_index: u64;
    // SAFETY: Reading `TPIDR_EL1` has no side effects. The kernel owns the register.
    unsafe {
        core::arch::asm!(
            "mrs {core_index}, tpidr_el1",
            core_index = out(reg) core_index,
            options(nomem, nostack, preserves_flags),
        );
    }

    return usize::try_from(core_index);
}

/// Records the kernel's index for this core in `TPIDR_EL1`.
pub fn write_current_core_index(core_index: usize) {
    // SAFETY: The kernel owns `TPIDR_EL1` and nothing else reads it.
    unsafe {
        core::arch::asm!(
            "msr tpidr_el1, {core_index}",
            core_index = in(reg) core_index as u64,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Allows interrupts on this core.
pub fn enable_interrupts() {
    // SAFETY: Clearing the IRQ mask bit only lets pending interrupts be taken.
    unsafe {
        core::arch::asm!("msr daifclr, #2", options(nostack, preserves_flags));
    }
}

/// Masks interrupts on this core and returns the previous mask state for `restore_interrupts`.
#[must_use]
pub fn mask_interrupts() -> u64 {
    let previous_state: u64;
    // SAFETY: Reading `DAIF` and setting the IRQ mask bit have no memory effects. The asm is not
    // marked `nomem`, so the compiler treats it as a barrier around the critical section.
    unsafe {
        core::arch::asm!(
            "mrs {previous_state}, daif",
            "msr daifset, #2",
            previous_state = out(reg) previous_state,
            options(nostack, preserves_flags),
        );
    }
    previous_state
}

/// Restores the interrupt mask state returned by `mask_interrupts`.
pub fn restore_interrupts(saved_state: u64) {
    // SAFETY: Writing back a value read from `DAIF` restores the earlier mask state.
    unsafe {
        core::arch::asm!(
            "msr daif, {saved_state}",
            saved_state = in(reg) saved_state,
            options(nostack, preserves_flags),
        );
    }
}
