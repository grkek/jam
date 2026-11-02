//! Exception vectors and the fault report.
//!
//! Interrupts are handed to the scheduler, which may resume a different frame. Faults taken at
//! EL0 end the faulting actor and resume the next frame. Every other exception ends in a report
//! on the console and a halt.

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

#[cfg(not(feature = "raspberry-pi-4"))]
core::arch::global_asm!(include_str!("exception_vectors.s"));

#[cfg(feature = "raspberry-pi-4")]
core::arch::global_asm!(include_str!("exception_vectors_raspberry_pi_4.s"));

unsafe extern "C" {
    /// First byte of the vector table, defined in `exception_vectors.s`.
    static exception_vector_table: u8;
}

/// Number of general purpose registers `x0` to `x30` saved in a [`TrapFrame`].
const GENERAL_REGISTER_COUNT: usize = 31;
/// How many registers are printed per row in the fault report.
const REGISTERS_PER_REPORT_ROW: usize = 4;

/// Bit position of the exception class within `ESR_EL1`.
const EXCEPTION_CLASS_SHIFT: u64 = 26;
/// Mask applied after shifting to extract the exception class.
const EXCEPTION_CLASS_MASK: u64 = 0x3f;
/// Number of vector entries per exception source in the vector table.
const VECTORS_PER_SOURCE: u64 = 4;
/// Vector index of an IRQ taken from the current level using `SP_ELx`.
const IRQ_CURRENT_LEVEL_SPX_VECTOR_INDEX: u64 = 5;
/// Vector index of an IRQ taken from a lower level running `AArch64`.
const IRQ_LOWER_AARCH64_VECTOR_INDEX: u64 = 9;
/// Vector index of a synchronous exception taken from a lower level running `AArch64`.
const LOWER_AARCH64_SYNCHRONOUS_VECTOR_INDEX: u64 = 8;

/// Exception class: unknown reason. Covers undefined instructions, including EL1-only
/// instructions and system register accesses attempted at EL0.
const EXCEPTION_CLASS_UNKNOWN: u64 = 0x00;
/// Exception class: trapped `WFI` or `WFE` instruction.
const EXCEPTION_CLASS_TRAPPED_WAIT: u64 = 0x01;
/// Exception class: floating-point or SIMD access while the unit is disabled.
const EXCEPTION_CLASS_TRAPPED_FLOATING_POINT: u64 = 0x07;
/// Exception class: illegal execution state.
const EXCEPTION_CLASS_ILLEGAL_EXECUTION_STATE: u64 = 0x0e;
/// Exception class: supervisor call (`SVC`) from `AArch64`.
const EXCEPTION_CLASS_SUPERVISOR_CALL: u64 = 0x15;
/// Exception class: trapped `MSR`, `MRS` or system instruction.
const EXCEPTION_CLASS_TRAPPED_SYSTEM_REGISTER: u64 = 0x18;
/// Exception class: instruction abort taken from a lower exception level.
const EXCEPTION_CLASS_INSTRUCTION_ABORT_LOWER_LEVEL: u64 = 0x20;
/// Exception class: instruction abort taken without a change of exception level.
const EXCEPTION_CLASS_INSTRUCTION_ABORT_SAME_LEVEL: u64 = 0x21;
/// Exception class: program counter alignment fault.
const EXCEPTION_CLASS_PROGRAM_COUNTER_ALIGNMENT: u64 = 0x22;
/// Exception class: data abort taken from a lower exception level.
const EXCEPTION_CLASS_DATA_ABORT_LOWER_LEVEL: u64 = 0x24;
/// Exception class: data abort taken without a change of exception level.
const EXCEPTION_CLASS_DATA_ABORT_SAME_LEVEL: u64 = 0x25;
/// Exception class: stack pointer alignment fault.
const EXCEPTION_CLASS_STACK_POINTER_ALIGNMENT: u64 = 0x26;
/// Exception class: trapped floating-point exception.
const EXCEPTION_CLASS_FLOATING_POINT_EXCEPTION: u64 = 0x2c;
/// Exception class: SError interrupt.
const EXCEPTION_CLASS_SERROR: u64 = 0x2f;
/// Exception class: `BRK` breakpoint instruction.
const EXCEPTION_CLASS_BREAKPOINT_INSTRUCTION: u64 = 0x3c;

/// Addresses below this value are reported as null-page accesses.
const NULL_PAGE_END: u64 = 0x1_0000;
/// First address of the kernel's upper-half mapping.
const KERNEL_SPACE_START: u64 = 0xffff_0000_0000_0000;

/// Mask for the instruction specific syndrome (ISS), the low 25 bits of `ESR_EL1`.
const INSTRUCTION_SPECIFIC_SYNDROME_MASK: u64 = 0x1ff_ffff;

/// ISS bit of a data abort: set when the faulting access was a write.
const WRITE_NOT_READ_BIT: u64 = 1 << 6;
/// ISS bit of a data abort: set when the fault came from a cache maintenance instruction.
const CACHE_MAINTENANCE_BIT: u64 = 1 << 8;
/// ISS bit of an abort: set when `FAR_EL1` does not hold a valid address.
const FAULT_ADDRESS_NOT_VALID_BIT: u64 = 1 << 10;

/// Mask for the fault status code (DFSC or IFSC) in the ISS of an abort.
const FAULT_STATUS_MASK: u64 = 0x3f;
/// Mask for the translation level held in the low bits of a fault status code.
const FAULT_LEVEL_MASK: u64 = 0b11;
/// Shift that turns a fault status code into its fault kind group.
const FAULT_KIND_SHIFT: u64 = 2;
/// Fault kind group: address size fault.
const FAULT_KIND_ADDRESS_SIZE: u64 = 0;
/// Fault kind group: translation fault.
const FAULT_KIND_TRANSLATION: u64 = 1;
/// Fault kind group: access flag fault.
const FAULT_KIND_ACCESS_FLAG: u64 = 2;
/// Fault kind group: permission fault.
const FAULT_KIND_PERMISSION: u64 = 3;
/// Fault status code: synchronous external abort that was not on a table walk.
const FAULT_STATUS_SYNCHRONOUS_EXTERNAL_ABORT: u64 = 0x10;
/// Fault status code: first of the synchronous external aborts on a table walk (level 0).
const FAULT_STATUS_TABLE_WALK_ABORT_FIRST: u64 = 0x14;
/// Fault status code: last of the synchronous external aborts on a table walk (level 3).
const FAULT_STATUS_TABLE_WALK_ABORT_LAST: u64 = 0x17;
/// Fault status code: alignment fault.
const FAULT_STATUS_ALIGNMENT: u64 = 0x21;
/// Fault status code: TLB conflict abort.
const FAULT_STATUS_TLB_CONFLICT: u64 = 0x30;

/// ISS bit of a trapped system register access: set for `MRS` (read), clear for `MSR` (write).
const SYSTEM_REGISTER_READ_BIT: u64 = 1;
/// Shift of the `CRm` field in the ISS of a trapped system register access.
const SYSTEM_REGISTER_CRM_SHIFT: u64 = 1;
/// Shift of the general register number (`Rt`) in the ISS of a trapped system register access.
const SYSTEM_REGISTER_RT_SHIFT: u64 = 5;
/// Shift of the `CRn` field in the ISS of a trapped system register access.
const SYSTEM_REGISTER_CRN_SHIFT: u64 = 10;
/// Shift of the `op1` field in the ISS of a trapped system register access.
const SYSTEM_REGISTER_OP1_SHIFT: u64 = 14;
/// Shift of the `op2` field in the ISS of a trapped system register access.
const SYSTEM_REGISTER_OP2_SHIFT: u64 = 17;
/// Shift of the `op0` field in the ISS of a trapped system register access.
const SYSTEM_REGISTER_OP0_SHIFT: u64 = 20;
/// Mask for the 2-bit `op0` field.
const SYSTEM_REGISTER_OP0_MASK: u64 = 0b11;
/// Mask for the 3-bit `op1` and `op2` fields.
const SYSTEM_REGISTER_OP_MASK: u64 = 0b111;
/// Mask for the 4-bit `CRn` and `CRm` fields.
const SYSTEM_REGISTER_CR_MASK: u64 = 0b1111;
/// Mask for the 5-bit general register number.
const SYSTEM_REGISTER_RT_MASK: u64 = 0b11111;

/// Mask for the 16-bit immediate of a `BRK` instruction in the ISS.
const BREAKPOINT_IMMEDIATE_MASK: u64 = 0xffff;

/// Names of the four exception sources, indexed by `vector_index / VECTORS_PER_SOURCE`.
const VECTOR_SOURCE_NAMES: [&str; 4] = [
    "current level, SP0",
    "current level, SPx",
    "lower level, AArch64",
    "lower level, AArch32",
];
/// Names of the four exception kinds, indexed by `vector_index % VECTORS_PER_SOURCE`.
const VECTOR_KIND_NAMES: [&str; 4] = ["synchronous", "IRQ", "FIQ", "SError"];

/// Everything needed to explain a synchronous fault in plain words.
///
/// Implements [`core::fmt::Display`], so it can be passed straight to the logging macros.
pub struct FaultDescription {
    /// The exception class (`ESR_EL1.EC`) identifying the kind of fault.
    pub exception_class: u64,
    /// The full `ESR_EL1` value.
    pub syndrome: u64,
    /// The `FAR_EL1` value; only meaningful for aborts and program counter alignment faults.
    pub fault_address: u64,
}

impl FaultDescription {
    /// Captures the syndrome and fault address from `frame`.
    pub fn new(frame: &TrapFrame, exception_class: u64) -> Self {
        Self {
            exception_class,
            syndrome: frame.exception_syndrome_register,
            fault_address: frame.fault_address_register,
        }
    }
}

impl core::fmt::Display for FaultDescription {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let specific_syndrome = self.syndrome & INSTRUCTION_SPECIFIC_SYNDROME_MASK;

        match self.exception_class {
            EXCEPTION_CLASS_DATA_ABORT_LOWER_LEVEL | EXCEPTION_CLASS_DATA_ABORT_SAME_LEVEL => {
                let access = if specific_syndrome & CACHE_MAINTENANCE_BIT != 0 {
                    "cache maintenance on"
                } else if specific_syndrome & WRITE_NOT_READ_BIT != 0 {
                    "write to"
                } else {
                    "read of"
                };
                write_memory_fault(
                    formatter,
                    "illegal memory access",
                    access,
                    specific_syndrome,
                    self.fault_address,
                )
            }
            EXCEPTION_CLASS_INSTRUCTION_ABORT_LOWER_LEVEL
            | EXCEPTION_CLASS_INSTRUCTION_ABORT_SAME_LEVEL => write_memory_fault(
                formatter,
                "illegal instruction fetch",
                "execute from",
                specific_syndrome,
                self.fault_address,
            ),
            EXCEPTION_CLASS_TRAPPED_SYSTEM_REGISTER => {
                let instruction = if specific_syndrome & SYSTEM_REGISTER_READ_BIT != 0 {
                    "MRS"
                } else {
                    "MSR"
                };
                write!(
                    formatter,
                    "privileged instruction: {instruction} S{}_{}_C{}_C{}_{} (x{})",
                    (specific_syndrome >> SYSTEM_REGISTER_OP0_SHIFT) & SYSTEM_REGISTER_OP0_MASK,
                    (specific_syndrome >> SYSTEM_REGISTER_OP1_SHIFT) & SYSTEM_REGISTER_OP_MASK,
                    (specific_syndrome >> SYSTEM_REGISTER_CRN_SHIFT) & SYSTEM_REGISTER_CR_MASK,
                    (specific_syndrome >> SYSTEM_REGISTER_CRM_SHIFT) & SYSTEM_REGISTER_CR_MASK,
                    (specific_syndrome >> SYSTEM_REGISTER_OP2_SHIFT) & SYSTEM_REGISTER_OP_MASK,
                    (specific_syndrome >> SYSTEM_REGISTER_RT_SHIFT) & SYSTEM_REGISTER_RT_MASK,
                )
            }
            EXCEPTION_CLASS_UNKNOWN => write!(
                formatter,
                "undefined instruction (includes privileged instructions executed at EL0)"
            ),
            EXCEPTION_CLASS_SUPERVISOR_CALL => write!(formatter, "supervisor call"),
            EXCEPTION_CLASS_TRAPPED_WAIT => write!(formatter, "trapped WFI/WFE instruction"),
            EXCEPTION_CLASS_TRAPPED_FLOATING_POINT => {
                write!(formatter, "floating-point/SIMD access while disabled")
            }
            EXCEPTION_CLASS_FLOATING_POINT_EXCEPTION => {
                write!(formatter, "floating-point exception")
            }
            EXCEPTION_CLASS_ILLEGAL_EXECUTION_STATE => {
                write!(formatter, "illegal execution state")
            }
            EXCEPTION_CLASS_PROGRAM_COUNTER_ALIGNMENT => {
                write!(
                    formatter,
                    "misaligned program counter {:#x}",
                    self.fault_address
                )
            }
            EXCEPTION_CLASS_STACK_POINTER_ALIGNMENT => {
                write!(formatter, "misaligned stack pointer")
            }
            EXCEPTION_CLASS_BREAKPOINT_INSTRUCTION => write!(
                formatter,
                "breakpoint instruction (BRK #{:#x})",
                specific_syndrome & BREAKPOINT_IMMEDIATE_MASK
            ),
            EXCEPTION_CLASS_SERROR => write!(formatter, "SError interrupt"),
            other => write!(formatter, "unhandled exception class {other:#x}"),
        }
    }
}

/// Writes a data or instruction abort as one readable sentence.
///
/// Decodes the fault status code from `specific_syndrome`, including the translation level
/// when the hardware reports one, and classifies `address` as null page, user or kernel.
fn write_memory_fault(
    formatter: &mut core::fmt::Formatter<'_>,
    headline: &str,
    access: &str,
    specific_syndrome: u64,
    address: u64,
) -> core::fmt::Result {
    let fault_status = specific_syndrome & FAULT_STATUS_MASK;
    let level = fault_status & FAULT_LEVEL_MASK;
    let address_is_valid = specific_syndrome & FAULT_ADDRESS_NOT_VALID_BIT == 0;

    let (name, meaning, reports_level) = match fault_status >> FAULT_KIND_SHIFT {
        FAULT_KIND_ADDRESS_SIZE => (
            "address size fault",
            "address outside the supported range",
            true,
        ),
        FAULT_KIND_TRANSLATION => ("translation fault", "address is not mapped", true),
        FAULT_KIND_ACCESS_FLAG => (
            "access flag fault",
            "mapping exists but is not marked accessed",
            true,
        ),
        FAULT_KIND_PERMISSION => (
            "permission fault",
            "mapped but not accessible in this way",
            true,
        ),
        _ => match fault_status {
            FAULT_STATUS_SYNCHRONOUS_EXTERNAL_ABORT => {
                ("synchronous external abort", "bus error", false)
            }
            FAULT_STATUS_TABLE_WALK_ABORT_FIRST..=FAULT_STATUS_TABLE_WALK_ABORT_LAST => (
                "external abort on table walk",
                "bus error while walking tables",
                true,
            ),
            FAULT_STATUS_ALIGNMENT => ("alignment fault", "misaligned access", false),
            FAULT_STATUS_TLB_CONFLICT => ("TLB conflict abort", "conflicting TLB entries", false),
            _ => ("unclassified fault status", "see ESR_EL1", false),
        },
    };

    write!(formatter, "{headline}: {access} ")?;
    if address_is_valid {
        write!(formatter, "{} {address:#x}", address_region(address))?;
    } else {
        write!(formatter, "an unknown address (FAR_EL1 not valid)")?;
    }
    write!(formatter, " ({name}")?;
    if reports_level {
        write!(formatter, ", level {level}")?;
    }
    write!(formatter, ": {meaning})")
}

/// Classifies a faulting address as null page, user or kernel for display.
fn address_region(address: u64) -> &'static str {
    if address < NULL_PAGE_END {
        "null-page address"
    } else if address >= KERNEL_SPACE_START {
        "kernel address"
    } else {
        "user address"
    }
}

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

/// Returns `names[index]`, or `"unknown"` when `index` is out of range.
fn name_at(names: &[&'static str], index: u64) -> &'static str {
    usize::try_from(index)
        .ok()
        .and_then(|position| names.get(position))
        .copied()
        .unwrap_or("unknown")
}

/// Validates the frame about to be resumed and halts the machine if it is unusable.
fn checked_return(frame: *mut TrapFrame) -> *mut TrapFrame {
    assert!(!frame.is_null(), "Null return frame");

    // SAFETY: The pointer was just checked to be non-null, and every caller passes a frame
    // produced by the vector entry, the scheduler or the dispatcher.
    let selected = unsafe { &*frame };
    let core = current_core_local().expect("Missing core");

    let slot = core.current_slot_index();

    if selected.exception_link_register == 0
        || (slot == NO_ACTOR && (selected.saved_program_status_register & 0xf) != 5)
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

    // Timer (and other) IRQs, whether taken from EL1 or EL0.
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

    // Any other synchronous exception from EL0 is that actor's own fault. End the actor, not
    // the machine.
    if frame.vector_index == LOWER_AARCH64_SYNCHRONOUS_VECTOR_INDEX {
        return checked_return(end_faulting_actor(frame, exception_class));
    }

    report_fault_and_halt(frame)
}

/// Reports a fault taken at EL0 and ends the actor that caused it.
fn end_faulting_actor(frame: &TrapFrame, exception_class: u64) -> *mut TrapFrame {
    let core = current_core_local()
        .map(|core_local| core_local.core_index())
        .unwrap_or(usize::MAX);
    let description = FaultDescription::new(frame, exception_class);

    let (status, packed_address) = current_address();

    if status == SystemCallStatus::Accepted {
        let address = LocalAddress::new(packed_address as u32, (packed_address >> 32) as u32);

        let _ = error!(
            EarlyConsole,
            "Actor {address} fault on core {core}: {description} (ELR_EL1 {:#x}, ESR_EL1 {:#x})",
            frame.exception_link_register,
            frame.exception_syndrome_register
        );
    } else {
        let _ = error!(
            EarlyConsole,
            "Unknown actor fault on core {core}: {description} (ELR_EL1 {:#x}, ESR_EL1 {:#x})",
            frame.exception_link_register,
            frame.exception_syndrome_register
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
    let description = FaultDescription::new(trap_frame, exception_class);
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
        "  EXC_CLS  {exception_class:#018x} ({description})"
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
