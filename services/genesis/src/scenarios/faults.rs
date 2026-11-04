//! Scenarios that die on purpose, each in a different way. The kernel must contain
//! every one of them to this actor.

use core::hint::black_box;

use crate::support::{fail, input_address};

pub fn breakpoint() -> ! {
    fail()
}

pub fn null_write() -> ! {
    // SAFETY: Deliberate fault.
    unsafe {
        core::ptr::write_volatile(core::ptr::without_provenance_mut::<u64>(0x10), 1);
    }
    fail()
}

pub fn execute_data() -> ! {
    // SAFETY: Deliberate permission fault: jumping into non-executable data.
    unsafe {
        core::arch::asm!("br {0}", in(reg) input_address(), options(noreturn));
    }
}

pub fn undefined_instruction() -> ! {
    // SAFETY: Deliberate undefined instruction.
    unsafe {
        core::arch::asm!("udf #0", options(noreturn));
    }
}

pub fn panic() -> ! {
    let nothing: Option<u8> = black_box(None);
    black_box(nothing.expect("deliberate panic"));
    fail()
}

fn recurse(depth: u64) -> u64 {
    let pad = black_box([depth as u8; 256]);
    if black_box(depth) == u64::MAX {
        return 0;
    }
    recurse(depth + 1).wrapping_add(u64::from(pad[0]))
}

pub fn stack_overflow() -> ! {
    black_box(recurse(0));
    fail()
}

pub fn write_code() -> ! {
    // SAFETY: Deliberate store to executable memory; write-xor-execute must refuse it.
    unsafe {
        core::ptr::write_volatile(write_code as usize as *mut u8, 0);
    }
    fail()
}
