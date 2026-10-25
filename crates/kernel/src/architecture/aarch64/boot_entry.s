// First instructions executed. QEMU starts only core zero; the others wait for PSCI.
// Assumes the kernel starts at EL1 with the MMU off, which is what QEMU `virt` provides.

	.section .text.boot
	.global _start
_start:
	// Any core other than core zero parks until the kernel brings it up.
	mrs	x0, mpidr_el1
	and	x0, x0, #0xff
	cbnz	x0, park

	// Stack for core zero.
	adrp	x0, __boot_stack_core_zero_top
	add	x0, x0, :lo12:__boot_stack_core_zero_top
	mov	sp, x0

	// Clear .bss, eight bytes at a time.
	adrp	x0, __bss_start
	add	x0, x0, :lo12:__bss_start
	adrp	x1, __bss_end
	add	x1, x1, :lo12:__bss_end
clear_bss:
	cmp	x0, x1
	b.hs	bss_cleared
	str	xzr, [x0], #8
	b	clear_bss
bss_cleared:

	bl	kernel_main

park:
	wfe
	b	park
