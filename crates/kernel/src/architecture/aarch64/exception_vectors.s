// The AArch64 exception vector table: 16 entries of 0x80 bytes, aligned to 0x800.
//
// Every entry saves the registers into a trap frame on the current stack and hands
// the frame, with the entry's index, to `exception_handler` in Rust.
//
// Index layout: source * 4 + kind
//   source: 0 current level SP0, 1 current level SPx, 2 lower level AArch64, 3 lower level AArch32
//   kind:   0 synchronous, 1 IRQ, 2 FIQ, 3 SError
//
// Trap frame (288 bytes, matches `TrapFrame` in exceptions.rs):
//   0..240  x0 to x30    248 ELR_EL1    256 SPSR_EL1
//   264 ESR_EL1          272 FAR_EL1    280 vector index

	.equ	TRAP_FRAME_SIZE, 288

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

	mov	x0, sp
	bl	exception_handler
	// The handler never returns yet. Park if it ever does.
1:	wfe
	b	1b
