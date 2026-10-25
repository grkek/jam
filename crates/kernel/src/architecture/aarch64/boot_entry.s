// First instructions executed. QEMU starts only core zero; the others wait for PSCI.
// Assumes the kernel starts at EL1 with the MMU off, which is what QEMU `virt` provides.

	.section .text.boot
	.global	_start
_start:
	// Keep the device tree address the firmware passed in x0.
	mov	x19, x0

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

	// kernel_main(device_tree_address)
	mov	x0, x19
	bl	kernel_main

park:
	wfe
	b	park

// Where a core started through PSCI begins.
// On entry x0 holds the context value from CPU_ON: the address of the kernel's translation tables.

	.section .text
	.global	secondary_entry
secondary_entry:
	mov	x19, x0

	// Core index, from the lowest affinity field.
	mrs	x20, mpidr_el1
	and	x20, x20, #0xff

	// Stack for this core: the top of stack slice number (index + 1).
	adrp	x1, __boot_stacks_bottom
	add	x1, x1, :lo12:__boot_stacks_bottom
	ldr	x2, =__boot_stack_size
	add	x3, x20, #1
	madd	x1, x3, x2, x1
	mov	sp, x1

	// secondary_main(core_index, translation_table_address)
	mov	x0, x20
	mov	x1, x19
	bl	secondary_main

secondary_park:
	wfe
	b	secondary_park
