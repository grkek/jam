//! Exception vectors and the fault report.
//!
//! Every exception currently ends in a report on the console and a halt. Resuming
//! after system calls and interrupts comes later.

use core::fmt::Write;

use super::early_console::EarlyConsole;
use crate::boot::entry::halt;

core::arch::global_asm!(include_str!("exception_vectors.s"));

unsafe extern "C" {
    /// First byte of the vector table, defined in `exception_vectors.s`.
    static exception_vector_table: u8;
}

const GENERAL_REGISTER_COUNT: usize = 31;
const REGISTERS_PER_REPORT_ROW: usize = 4;

const EXCEPTION_CLASS_SHIFT: u64 = 26;
const EXCEPTION_CLASS_MASK: u64 = 0x3f;
const VECTORS_PER_SOURCE: u64 = 4;

const EXCEPTION_CLASS_UNKNOWN: u64 = 0x00;
const EXCEPTION_CLASS_SUPERVISOR_CALL: u64 = 0x15;
const EXCEPTION_CLASS_INSTRUCTION_ABORT_LOWER_LEVEL: u64 = 0x20;
const EXCEPTION_CLASS_INSTRUCTION_ABORT_SAME_LEVEL: u64 = 0x21;
const EXCEPTION_CLASS_PROGRAM_COUNTER_ALIGNMENT: u64 = 0x22;
const EXCEPTION_CLASS_DATA_ABORT_LOWER_LEVEL: u64 = 0x24;
const EXCEPTION_CLASS_DATA_ABORT_SAME_LEVEL: u64 = 0x25;
const EXCEPTION_CLASS_STACK_POINTER_ALIGNMENT: u64 = 0x26;
const EXCEPTION_CLASS_SERROR: u64 = 0x2f;
const EXCEPTION_CLASS_BREAKPOINT_INSTRUCTION: u64 = 0x3c;

const VECTOR_SOURCE_NAMES: [&str; 4] = [
    "current level, SP0",
    "current level, SPx",
    "lower level, AArch64",
    "lower level, AArch32",
];
const VECTOR_KIND_NAMES: [&str; 4] = ["synchronous", "IRQ", "FIQ", "SError"];

/// Registers saved by the vector entry, laid out exactly as `exception_vectors.s` writes them.
#[repr(C)]
pub struct TrapFrame {
    /// `x0` to `x30` as they were when the exception was taken.
    pub general_registers: [u64; GENERAL_REGISTER_COUNT],
    /// `ELR_EL1`: where execution was when the exception was taken.
    pub exception_link_register: u64,
    /// `SPSR_EL1`: the processor state at that moment.
    pub saved_program_status_register: u64,
    /// `ESR_EL1`: why the exception happened.
    pub exception_syndrome_register: u64,
    /// `FAR_EL1`: the faulting address, when the exception has one.
    pub fault_address_register: u64,
    /// Which of the 16 vector entries was taken.
    pub vector_index: u64,
}

/// Installs the vector table on the calling core.
pub fn install_exception_vectors() {
    // SAFETY: `exception_vector_table` is defined by the assembly in this crate and is aligned
    // to 0x800, as `VBAR_EL1` requires. Writing `VBAR_EL1` at EL1 has no memory side effects.
    unsafe {
        let table_address = &raw const exception_vector_table as u64;
        core::arch::asm!(
            "msr vbar_el1, {table_address}",
            "isb",
            table_address = in(reg) table_address,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Executes a breakpoint instruction, which must end in a fault report.
///
/// Temporary: it proves the vector table works. Remove it once real faults exercise it.
pub fn trigger_test_breakpoint() {
    // SAFETY: `brk` raises a synchronous exception that the installed vectors handle.
    unsafe {
        core::arch::asm!("brk #0");
    }
}

fn name_at(names: &[&'static str], index: u64) -> &'static str {
    usize::try_from(index)
        .ok()
        .and_then(|position| names.get(position))
        .copied()
        .unwrap_or("unknown")
}

fn exception_class_name(exception_class: u64) -> &'static str {
    match exception_class {
        EXCEPTION_CLASS_UNKNOWN => "unknown reason",
        EXCEPTION_CLASS_SUPERVISOR_CALL => "supervisor call from AArch64",
        EXCEPTION_CLASS_INSTRUCTION_ABORT_LOWER_LEVEL => "instruction abort from a lower level",
        EXCEPTION_CLASS_INSTRUCTION_ABORT_SAME_LEVEL => "instruction abort at the same level",
        EXCEPTION_CLASS_PROGRAM_COUNTER_ALIGNMENT => "program counter alignment fault",
        EXCEPTION_CLASS_DATA_ABORT_LOWER_LEVEL => "data abort from a lower level",
        EXCEPTION_CLASS_DATA_ABORT_SAME_LEVEL => "data abort at the same level",
        EXCEPTION_CLASS_STACK_POINTER_ALIGNMENT => "stack pointer alignment fault",
        EXCEPTION_CLASS_SERROR => "SError interrupt",
        EXCEPTION_CLASS_BREAKPOINT_INSTRUCTION => "breakpoint instruction",
        _ => "other",
    }
}

/// Reports the exception on the console and halts. Called from `exception_vectors.s`.
#[unsafe(no_mangle)]
pub extern "C" fn exception_handler(trap_frame: &TrapFrame) -> ! {
    let exception_class =
        (trap_frame.exception_syndrome_register >> EXCEPTION_CLASS_SHIFT) & EXCEPTION_CLASS_MASK;
    let source_name = name_at(
        &VECTOR_SOURCE_NAMES,
        trap_frame.vector_index / VECTORS_PER_SOURCE,
    );
    let kind_name = name_at(
        &VECTOR_KIND_NAMES,
        trap_frame.vector_index % VECTORS_PER_SOURCE,
    );

    let mut console = EarlyConsole;
    let _ = writeln!(console, "\nEXCEPTION: {kind_name} from {source_name}");
    let _ = writeln!(
        console,
        "  EXC_CLS  {exception_class:#018x} ({})",
        exception_class_name(exception_class)
    );
    let _ = writeln!(
        console,
        "  ESR_EL1  {:#018x}",
        trap_frame.exception_syndrome_register
    );
    let _ = writeln!(
        console,
        "  FAR_EL1  {:#018x}",
        trap_frame.fault_address_register
    );
    let _ = writeln!(
        console,
        "  ELR_EL1  {:#018x}",
        trap_frame.exception_link_register
    );
    let _ = writeln!(
        console,
        "  SPSR_EL1 {:#018x}",
        trap_frame.saved_program_status_register
    );

    for (row, row_registers) in trap_frame
        .general_registers
        .chunks(REGISTERS_PER_REPORT_ROW)
        .enumerate()
    {
        for (column, register_value) in row_registers.iter().enumerate() {
            let register_number = row * REGISTERS_PER_REPORT_ROW + column;
            let _ = write!(console, "  x{register_number:<2} {register_value:#018x}");
        }
        let _ = writeln!(console);
    }
    halt()
}
