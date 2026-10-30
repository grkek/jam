//! Exception vectors and the fault report.
//!
//! Interrupts are handed to the scheduler, which may resume a different frame. Every other
//! exception ends in a report on the console and a halt.

use core::fmt::Write;

use foundation::{address::LocalAddress, cause::Cause, system_call_status::SystemCallStatus};

use super::{early_console::EarlyConsole, interrupts::handle_interrupt};
use crate::{
    actors::lifecycle,
    architecture::aarch64::power_interface,
    error,
    scheduling::core_local::{NO_ACTOR, current_core_local},
    system_calls::{actor_calls::current_address, dispatcher},
};

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
const IRQ_CURRENT_LEVEL_SPX_VECTOR_INDEX: u64 = 5;
const IRQ_LOWER_AARCH64_VECTOR_INDEX: u64 = 9;
const LOWER_AARCH64_SYNCHRONOUS_VECTOR_INDEX: u64 = 8;

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
    /// `SP_EL0`: the stack pointer used at EL0.
    pub stack_pointer_el0: u64,
    /// Keeps the frame a multiple of 16 bytes.
    pub _padding: u64,
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
        EXCEPTION_CLASS_SUPERVISOR_CALL => "supervisor call from `AArch64`",
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

fn checked_return(frame: *mut TrapFrame) -> *mut TrapFrame {
    assert!(!frame.is_null(), "Null return frame");

    let selected = unsafe { &*frame };
    let core = crate::scheduling::core_local::current_core_local().expect("Missing core");

    let slot = core.current_slot_index();

    if selected.exception_link_register == 0
        || (slot == crate::scheduling::core_local::NO_ACTOR
            && (selected.saved_program_status_register & 0xf) != 5)
    {
        error!(
            EarlyConsole,
            "BAD RETURN: core={} slot={} frame={:p} PC={:#x} SPSR={:#x}",
            core.core_index(),
            slot,
            frame,
            selected.exception_link_register,
            selected.saved_program_status_register,
        );

        power_interface::halt_machine();
    }

    frame
}

/// Called from `exception_vectors.s` on the core's interrupt stack. Returns the frame to resume.
#[unsafe(no_mangle)]
pub extern "C" fn exception_handler(trap_frame: *mut TrapFrame) -> *mut TrapFrame {
    if let Some(core) = current_core_local() {
        let slot = core.current_slot_index();
        let idle_frame = core.idle_frame_address();

        if slot != NO_ACTOR && trap_frame as usize == idle_frame {
            error!(
                EarlyConsole,
                "FRAME COLLISION: core={} actor_slot={} frame={:#x}",
                core.core_index(),
                slot,
                idle_frame,
            );

            power_interface::halt_machine();
        }
    }

    // SAFETY: The vector entry passes the frame it just saved on the interrupted stack.
    let frame = unsafe { &*trap_frame };

    // Timer (and other) IRQs — whether taken from EL1 or EL0.
    if frame.vector_index == IRQ_CURRENT_LEVEL_SPX_VECTOR_INDEX
        || frame.vector_index == IRQ_LOWER_AARCH64_VECTOR_INDEX
    {
        return checked_return(handle_interrupt(trap_frame as usize) as *mut TrapFrame);
    }

    let exception_class =
        (frame.exception_syndrome_register >> EXCEPTION_CLASS_SHIFT) & EXCEPTION_CLASS_MASK;

    if frame.vector_index == LOWER_AARCH64_SYNCHRONOUS_VECTOR_INDEX
        && exception_class == EXCEPTION_CLASS_SUPERVISOR_CALL
    {
        return checked_return(dispatcher::dispatch(trap_frame));
    }

    // Any other synchronous exception from EL0 is that actor's own fault. End the actor, not the machine.
    if frame.vector_index == LOWER_AARCH64_SYNCHRONOUS_VECTOR_INDEX {
        return checked_return(end_faulting_actor(frame, exception_class));
    }

    report_fault_and_halt(frame)
}

/// Reports a fault taken at EL0 and ends the actor that caused it.
/// Reports a fault taken at EL0 and ends the actor that caused it.
fn end_faulting_actor(frame: &TrapFrame, exception_class: u64) -> *mut TrapFrame {
    let core = crate::scheduling::core_local::current_core_local()
        .map(|core_local| core_local.core_index())
        .unwrap_or(usize::MAX);

    let (status, packed_address) = current_address();

    if status == SystemCallStatus::Accepted {
        let address = LocalAddress::new(packed_address as u32, (packed_address >> 32) as u32);

        let _ = error!(
            EarlyConsole,
            "Actor {address} fault on core {core}: {} (ESR_EL1 {:#x}, FAR_EL1 {:#x}, ELR_EL1 {:#x})",
            exception_class_name(exception_class),
            frame.exception_syndrome_register,
            frame.fault_address_register,
            frame.exception_link_register
        );
    } else {
        let _ = error!(
            EarlyConsole,
            "Unknown actor fault on core {core}: {} (ESR_EL1 {:#x}, FAR_EL1 {:#x}, ELR_EL1 {:#x})",
            exception_class_name(exception_class),
            frame.exception_syndrome_register,
            frame.fault_address_register,
            frame.exception_link_register
        );
    }

    match lifecycle::end_current_actor(Cause::Faulted) {
        Some(next_frame) => next_frame as *mut TrapFrame,
        None => report_fault_and_halt(frame),
    }
}

/// Reports the exception on the console and halts.
fn report_fault_and_halt(trap_frame: &TrapFrame) -> ! {
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

    power_interface::halt_machine()
}
