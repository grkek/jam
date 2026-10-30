//! Genesis fault-injection actor for validating JAM's EL0 isolation.
//!
//! Select one test using the constants below. Every test should terminate
//! Genesis without bringing down EL1 or another actor.

#![no_std]
#![no_main]

use core::{arch::asm, hint::spin_loop, panic::PanicInfo};
use foundation::system_call::SystemCall;

/// Read a kernel-space address through the upper-half mapping.
const TEST_KERNEL_READ: u8 = 1;
/// Write to an unmapped lower-half virtual address.
const TEST_INVALID_WRITE: u8 = 2;
/// Attempt an EL1-only system-register operation from EL0.
const TEST_PRIVILEGED_INSTRUCTION: u8 = 3;
/// Invoke a deliberately unknown system-call number.
const TEST_UNKNOWN_SYSTEM_CALL: u8 = 4;
/// Exit normally to confirm the control case.
const TEST_NORMAL_EXIT: u8 = 5;

/// Change this value to run a different fault-injection scenario.
const SELECTED_TEST: u8 = TEST_KERNEL_READ;

/// A panic must not return into an invalid actor context.
#[panic_handler]
fn on_panic(_information: &PanicInfo) -> ! {
    park()
}

/// Entry point invoked by JAM's ELF actor loader.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    match SELECTED_TEST {
        TEST_KERNEL_READ => read_kernel_memory(),
        TEST_INVALID_WRITE => write_invalid_memory(),
        TEST_PRIVILEGED_INSTRUCTION => execute_privileged_instruction(),
        TEST_UNKNOWN_SYSTEM_CALL => invoke_unknown_system_call(),
        TEST_NORMAL_EXIT => finish(),
        _ => park(),
    }
}

/// An EL0 actor must never read the kernel's upper-half direct map.
fn read_kernel_memory() -> ! {
    // SAFETY: Intentional invalid access for an isolated QEMU fault test.
    unsafe {
        core::ptr::read_volatile(0xffff_ff80_4020_0000usize as *const u64);
    }
    park()
}

/// An unmapped EL0 address must cause an actor-local data abort.
fn write_invalid_memory() -> ! {
    // SAFETY: Intentional invalid access for an isolated QEMU fault test.
    unsafe {
        core::ptr::write_volatile(0x0000_0000_ffff_f000usize as *mut u64, 0xdead_beef);
    }
    park()
}

/// EL0 must not be allowed to modify the EL1 translation base.
fn execute_privileged_instruction() -> ! {
    // SAFETY: Intentionally traps when executed from EL0.
    unsafe {
        asm!("msr ttbr1_el1, xzr", options(nostack));
    }
    park()
}

/// Unknown calls must be rejected without corrupting kernel state.
fn invoke_unknown_system_call() -> ! {
    // SAFETY: This deliberately supplies an unsupported system-call number.
    unsafe {
        asm!("svc #0", in("x8") u64::MAX, options(nostack));
    }
    park()
}

/// Normal actor termination is the control case.
fn finish() -> ! {
    // SAFETY: JAM's FinishActor system call does not return.
    unsafe {
        asm!("svc #0", in("x8") SystemCall::FinishActor as u64, options(noreturn));
    }
}

/// Prevents accidental fallthrough after a fault test unexpectedly returns.
fn park() -> ! {
    loop {
        spin_loop();
    }
}
