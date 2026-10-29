// The `AArch64` exception vector table: 16 entries of 0x80 bytes, aligned to 0x800.
//
// Every entry saves the registers into a trap frame on the current stack, switches to the
// core's own interrupt stack, and hands the frame, with the entry's index, to
// `exception_handler` in Rust. The handler returns the frame to resume, which is normally the
// same one. Resuming a different frame is how the scheduler switches actors.
//
// Index layout: source * 4 + kind
//   source: 0 current level SP0, 1 current level SPx, 2 lower level `AArch64`, 3 lower level AArch32
//   kind:   0 synchronous, 1 IRQ, 2 FIQ, 3 SError
//
// Trap frame (304 bytes, matches `TrapFrame` in exceptions.rs):
//   0..248  x0 to x30    248 ELR_EL1    256 SPSR_EL1
//   264 ESR_EL1          272 FAR_EL1    280 vector index
//   288 SP_EL0           296 padding to 16-byte size

	.equ	TRAP_FRAME_SIZE, 304

	.macro	vector_entry index
	.balign	0x80
	sub	sp, sp, #TRAP_FRAME_SIZE
	stp	x0, x1, [sp, #0]
	mov	x0, #\index
	b	exception_common
	.endm

	.section .text.exception_vectors, "ax"
	.balign	0x800
	.global	exception_vector_table
exception_vector_table:
	vector_entry 0
	vector_entry 1
	vector_entry 2
	vector_entry 3
	vector_entry 4
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
exception_common:
	// x0 holds the vector index. The original x0 and x1 are already saved.
	stp	x2, x3, [sp, #16]
	stp	x4, x5, [sp, #32]
	stp	x6, x7, [sp, #48]
	stp	x8, x9, [sp, #64]
	stp	x10, x11, [sp, #80]
	stp	x12, x13, [sp, #96]
	stp	x14, x15, [sp, #112]
	stp	x16, x17, [sp, #128]
	stp	x18, x19, [sp, #144]
	stp	x20, x21, [sp, #160]
	stp	x22, x23, [sp, #176]
	stp	x24, x25, [sp, #192]
	stp	x26, x27, [sp, #208]
	stp	x28, x29, [sp, #224]
	str	x30, [sp, #240]

	mrs	x2, elr_el1
	mrs	x3, spsr_el1
	stp	x2, x3, [sp, #248]
	mrs	x2, esr_el1
	mrs	x3, far_el1
	stp	x2, x3, [sp, #264]
	str	x0, [sp, #280]
	mrs	x2, sp_el0
	str	x2, [sp, #288]

	// Run the handler on this core's interrupt stack, so the interrupted stack stays untouched
	// and another core can safely resume it once the frame is saved.
	mrs	x1, tpidr_el1
	add	x1, x1, #1
	ldr x2, =__interrupt_stack_stride
	adrp	x3, __interrupt_stacks_bottom
	add	x3, x3, :lo12:__interrupt_stacks_bottom
	madd	x3, x1, x2, x3
	mov	x0, sp
	mov	sp, x3
	bl	exception_handler

	// x0 is the frame to resume.
	mov	sp, x0
	ldp	x2, x3, [sp, #248]
	msr	elr_el1, x2
	msr	spsr_el1, x3
	ldr	x2, [sp, #288]
	msr	sp_el0, x2
	ldr	x30, [sp, #240]
	ldp	x28, x29, [sp, #224]
	ldp	x26, x27, [sp, #208]
	ldp	x24, x25, [sp, #192]
	ldp	x22, x23, [sp, #176]
	ldp	x20, x21, [sp, #160]
	ldp	x18, x19, [sp, #144]
	ldp	x16, x17, [sp, #128]
	ldp	x14, x15, [sp, #112]
	ldp	x12, x13, [sp, #96]
	ldp	x10, x11, [sp, #80]
	ldp	x8, x9, [sp, #64]
	ldp	x6, x7, [sp, #48]
	ldp	x4, x5, [sp, #32]
	ldp	x2, x3, [sp, #16]
	ldp	x0, x1, [sp, #0]
	add	sp, sp, #TRAP_FRAME_SIZE
	eret
