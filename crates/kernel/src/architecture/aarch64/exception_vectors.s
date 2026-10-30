// AArch64 exception vectors: 16 entries of 128 bytes, aligned to 2048 bytes.
//
// Ordinary entries save a 304-byte TrapFrame on the interrupted stack, then
// dispatch on the per-core interrupt stack. EL1 synchronous exceptions instead
// request an immediate machine shutdown without touching the interrupted stack.
//
// Vector index = source * 4 + kind:
//   source 0: current EL, SP_EL0; source 1: current EL, SP_EL1;
//   source 2: lower EL, AArch64;  source 3: lower EL, AArch32.
//   kind 0: synchronous; 1: IRQ; 2: FIQ; 3: SError.
//
// TrapFrame: x0-x30 [0..247], ELR [248], SPSR [256], ESR [264], FAR [272],
// vector index [280], SP_EL0 [288], padding [296]; total 304 bytes.

    .equ TRAP_FRAME_SIZE, 304

    // Normal entry; the interrupted stack must have room for a TrapFrame.
    .macro vector_entry index
    .balign 0x80
    sub sp, sp, #TRAP_FRAME_SIZE
    stp x0, x1, [sp, #0]
    mov x0, #\index
    b exception_common
    .endm

    // Fatal EL1 synchronous entry; it must not read or write through SP.
    .macro fatal_vector_entry
    .balign 0x80
    b kernel_fatal_halt
    .endm

    .section .text.exception_vectors, "ax"
    .balign 0x800
    .global exception_vector_table
exception_vector_table:
    fatal_vector_entry  // EL1 using SP_EL0: synchronous exception.
    vector_entry 1
    vector_entry 2
    vector_entry 3
    fatal_vector_entry  // EL1 using SP_EL1: synchronous exception.
    vector_entry 5
    vector_entry 6
    vector_entry 7
    vector_entry 8
    vector_entry 9
    vector_entry 10
    vector_entry 11
    vector_entry 12
    vector_entry 13
    vector_entry 14
    vector_entry 15

    .section .text

// Fail closed on any EL1 synchronous exception, including a stack-guard abort.
// This QEMU virt kernel uses PSCI over HVC; SYSTEM_OFF shuts down all vCPUs.
// No Rust calls, locks, diagnostics, register saves, or stack accesses occur.
kernel_fatal_halt:
    msr daifset, #0xf

    // PSCI SYSTEM_OFF function identifier: 0x84000008.
    mov w0, #8
    movk w0, #0x8400, lsl #16
    hvc #0

    // Park this core if the firmware refuses or returns unexpectedly.
1:
    wfe
    b 1b

exception_common:
    // Original x0 and x1 are already saved in the interrupted TrapFrame.
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]
    stp x6, x7, [sp, #48]
    stp x8, x9, [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    str x30, [sp, #240]

    // Capture the exception state before calling into Rust.
    mrs x2, elr_el1
    mrs x3, spsr_el1
    stp x2, x3, [sp, #248]
    mrs x2, esr_el1
    mrs x3, far_el1
    stp x2, x3, [sp, #264]
    str x0, [sp, #280]
    mrs x2, sp_el0
    str x2, [sp, #288]

    // TPIDR_EL1 stores core_index - 1; IRQ stacks have linker-defined strides.
    mrs x1, tpidr_el1
    add x1, x1, #1
    ldr x2, =__interrupt_stack_stride
    adrp x3, __interrupt_stacks_bottom
    add x3, x3, :lo12:__interrupt_stacks_bottom
    madd x3, x1, x2, x3
    mov x0, sp
    mov sp, x3
    bl exception_handler

    // Restore the selected frame, possibly belonging to another actor.
    mov sp, x0
    ldp x2, x3, [sp, #248]
    msr elr_el1, x2
    msr spsr_el1, x3
    ldr x2, [sp, #288]
    msr sp_el0, x2
    ldr x30, [sp, #240]
    ldp x28, x29, [sp, #224]
    ldp x26, x27, [sp, #208]
    ldp x24, x25, [sp, #192]
    ldp x22, x23, [sp, #176]
    ldp x20, x21, [sp, #160]
    ldp x18, x19, [sp, #144]
    ldp x16, x17, [sp, #128]
    ldp x14, x15, [sp, #112]
    ldp x12, x13, [sp, #96]
    ldp x10, x11, [sp, #80]
    ldp x8, x9, [sp, #64]
    ldp x6, x7, [sp, #48]
    ldp x4, x5, [sp, #32]
    ldp x2, x3, [sp, #16]
    ldp x0, x1, [sp, #0]
    add sp, sp, #TRAP_FRAME_SIZE
    eret
