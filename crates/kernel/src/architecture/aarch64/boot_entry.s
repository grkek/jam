// First instructions executed, with translation off, at physical addresses.
//
// Core zero builds two small translation tables, turns translation on, and jumps to the
// kernel's upper-half address. Cores started later reuse the same tables. Everything between
// `_start` and the jump to `high_entry` must run at its physical address: it cannot use
// pc-relative references to the upper half, only absolute literals.
//
// Boot tables: both use 1 GiB blocks (39-bit addresses, 4 KiB granule, level one).
//   TTBR0 (identity):  0x4000_0000 and 0x8000_0000 -> the same physical addresses, normal memory.
//   TTBR1 (upper half): KERNEL_OFFSET + physical address for 0 to 1 GiB (devices) and
//                       1 GiB to 3 GiB (RAM). The Rust kernel replaces these with strict tables.

	.equ	BOOT_TABLE_BYTE_COUNT, 8192
	.equ	NORMAL_BLOCK_ATTRIBUTES, 0x705
	.equ	SCTLR_MMU_AND_CACHES, 0x1005

	.section .text.boot, "ax"
	.global	_start
_start:
	// Keep the device tree address the firmware passed in x0.
	mov	x19, x0

	// Any core other than core zero parks until the kernel brings it up.
	mrs	x0, mpidr_el1
	and	x0, x0, #0xff
	cbnz	x0, park

	bl	build_boot_tables
	bl	enable_boot_mmu

	// Translation is on and we still run at the physical address through the identity table.
	// Jump to the upper half with an absolute address.
	ldr	x0, =high_entry
	br	x0

park:
	wfe
	b	park

// Writes the two boot tables. Uses only registers.
build_boot_tables:
	adrp	x0, __boot_tables_start
	add	x0, x0, :lo12:__boot_tables_start
	mov	x1, #BOOT_TABLE_BYTE_COUNT
	add	x2, x0, x1
clear_boot_tables:
	stp	xzr, xzr, [x0], #16
	cmp	x0, x2
	b.lo	clear_boot_tables

	adrp	x0, __boot_tables_start
	add	x0, x0, :lo12:__boot_tables_start
	add	x1, x0, #4096

	// TTBR0 table: identity map of the first two gigabytes of RAM.
	mov	x2, #NORMAL_BLOCK_ATTRIBUTES
	mov	x3, #0x40000000
	orr	x3, x3, x2
	str	x3, [x0, #8]
	mov	x3, #0x80000000
	orr	x3, x3, x2
	str	x3, [x0, #16]

	// TTBR1 table: devices at index 0, RAM at indices 1 and 2.
	ldr	x3, =0x0060000000000601
	str	x3, [x1, #0]
	mov	x3, #0x40000000
	orr	x3, x3, x2
	str	x3, [x1, #8]
	mov	x3, #0x80000000
	orr	x3, x3, x2
	str	x3, [x1, #16]

	dsb	ish
	ret

// Programs the translation registers and turns on translation and the caches. Uses only registers.
enable_boot_mmu:
	adrp	x0, __boot_tables_start
	add	x0, x0, :lo12:__boot_tables_start
	add	x1, x0, #4096
	msr	ttbr0_el1, x0
	msr	ttbr1_el1, x1

	// MAIR_EL1: attribute 0 is device memory, attribute 1 is normal write-back memory.
	mov	x2, #0xff00
	msr	mair_el1, x2

	// TCR_EL1: 39-bit addresses in both halves, 4 KiB granules, write-back, inner shareable.
	// The physical address size comes from the hardware, capped at 48 bits.
	ldr	x2, =0xB5193519
	mrs	x3, id_aa64mmfr0_el1
	and	x3, x3, #0xf
	mov	x4, #5
	cmp	x3, x4
	csel	x3, x3, x4, lo
	orr	x2, x2, x3, lsl #32
	msr	tcr_el1, x2
	isb

	tlbi	vmalle1
	ic	iallu
	dsb	ish
	isb

	mrs	x2, sctlr_el1
	mov	x3, #SCTLR_MMU_AND_CACHES
	orr	x2, x2, x3
	msr	sctlr_el1, x2
	isb
	ret

// Where a core started through PSCI begins, at its physical address.
// On entry x0 holds the context value from CPU_ON: the physical address of the kernel's tables.
	.global	secondary_entry
secondary_entry:
	mov	x19, x0
	bl	enable_boot_mmu
	ldr	x0, =secondary_high_entry
	br	x0

	.section .text, "ax"

// The first code that runs in the upper half on core zero.
high_entry:
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

	// kernel_main(device_tree_address), where the address is still physical.
	mov	x0, x19
	bl	kernel_main

high_park:
	wfe
	b	high_park

// The first code that runs in the upper half on a secondary core.
secondary_high_entry:
	// Core index, from the lowest affinity field.
	mrs	x20, mpidr_el1
	and	x20, x20, #0xff

	// Stack for this core: the top of stack slice number (index + 1).
	adrp	x1, __boot_stacks_bottom
	add	x1, x1, :lo12:__boot_stacks_bottom
	ldr x2, =__boot_stack_stride
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

// The physical address of `secondary_entry`, as data, because code in the upper half cannot
// form that address itself: it is too far away for a pc-relative reference.
	.section .rodata
	.balign	8
	.global	secondary_entry_physical_address
secondary_entry_physical_address:
	.quad	secondary_entry
