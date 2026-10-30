// First-stage Raspberry Pi 4 Model B boot code.
//
// The VideoCore firmware loads `kernel8.img` at physical 0x0008_0000 and
// passes the physical FDT address in x0. Entry may be at EL2 or EL1.
// Before the first Rust call, this stub moves to EL1, establishes a temporary
// physical identity map and upper-half map, and clears BSS on the boot core.
//
// This stage intentionally uses core zero only. A separate Raspberry Pi
// secondary-core release protocol is required before enabling SMP.
//
// Temporary 1 GiB descriptors (4 KiB granule, level 1):
//  TTBR0[0] = physical 0..1 GiB as Normal RAM, for the boot stub.
//  TTBR1[0] = physical 0..1 GiB as Normal RAM, for the kernel image.
//  TTBR1[3] = physical 3..4 GiB as Device memory, for BCM2711 MMIO.
// The strict Rust mapper subsequently installs page-precise kernel mappings.

    .equ BOOT_TABLE_BYTE_COUNT, 8192
    .equ NORMAL_BLOCK_ATTRIBUTES, 0x705
    .equ SCTLR_MMU_AND_CACHES, 0x1005
    .equ EXCEPTION_LEVEL_ONE, 4
    .equ EXCEPTION_LEVEL_TWO, 8

    .section .text.boot, "ax"
    .global _start
_start:
    // x19 remains untouched by the page-table helper routines.
    mov x19, x0

    mrs x0, mpidr_el1
    and x0, x0, #0xff
    cbnz x0, park_secondary_core

    mrs x0, currentel
    and x0, x0, #0xc
    cmp x0, #EXCEPTION_LEVEL_ONE
    b.eq begin_exception_level_one
    cmp x0, #EXCEPTION_LEVEL_TWO
    b.ne park_secondary_core

    // Permit AArch64 EL1 and the physical/virtual architectural counters.
    // No memory or stack is accessed before ERET.
    mov x0, #0x80000000
    msr hcr_el2, x0
    mov x0, #3
    msr cnthctl_el2, x0
    msr cntvoff_el2, xzr

    // Enter EL1h with Debug, SError, IRQ, and FIQ masked.
    mov x0, #0x3c5
    msr spsr_el2, x0
    adr x0, begin_exception_level_one
    msr elr_el2, x0
    eret

begin_exception_level_one:
    msr daifset, #0xf
    bl build_boot_tables
    bl enable_boot_mmu

    // Page-table walks now work; enter the upper-half linked kernel.
    ldr x0, =high_entry
    br x0

park_secondary_core:
    wfe
    b park_secondary_core

build_boot_tables:
    adrp x0, __boot_tables_start
    add x0, x0, :lo12:__boot_tables_start
    mov x1, #BOOT_TABLE_BYTE_COUNT
    add x2, x0, x1
clear_boot_tables:
    stp xzr, xzr, [x0], #16
    cmp x0, x2
    b.lo clear_boot_tables

    adrp x0, __boot_tables_start
    add x0, x0, :lo12:__boot_tables_start
    add x1, x0, #4096

    // Identity map RAM below 1 GiB.
    mov x2, #NORMAL_BLOCK_ATTRIBUTES
    str x2, [x0]

    // Upper-half direct map for low RAM.
    str x2, [x1]

    // Upper-half device map for BCM2711 low-peripheral-mode MMIO.
    ldr x2, =0x0060000000000601
    mov x3, #0xc0000000
    orr x2, x2, x3
    str x2, [x1, #24]

    dsb ish
    ret

enable_boot_mmu:
    adrp x0, __boot_tables_start
    add x0, x0, :lo12:__boot_tables_start
    add x1, x0, #4096
    msr ttbr0_el1, x0
    msr ttbr1_el1, x1

    // MAIR: attribute zero = Device-nGnRnE, attribute one = Normal WB.
    mov x2, #0xff00
    msr mair_el1, x2

    // 39-bit halves, 4 KiB granules, inner shareable, write-back caches.
    // Physical address size follows the CPU but does not exceed 48 bits.
    ldr x2, =0xB5193519
    mrs x3, id_aa64mmfr0_el1
    and x3, x3, #0xf
    mov x4, #5
    cmp x3, x4
    csel x3, x3, x4, lo
    orr x2, x2, x3, lsl #32
    msr tcr_el1, x2
    isb

    tlbi vmalle1
    ic iallu
    dsb ish
    isb

    mrs x2, sctlr_el1
    mov x3, #SCTLR_MMU_AND_CACHES
    orr x2, x2, x3
    msr sctlr_el1, x2
    isb
    ret

// Kept for the existing linker-visible PSCI ABI. On Raspberry Pi firmware
// without a PSCI conduit, JAM does not attempt to start secondary cores.
    .global secondary_entry
secondary_entry:
    wfe
    b secondary_entry

    .section .text, "ax"
high_entry:
    adrp x0, __boot_stack_core_zero_top
    add x0, x0, :lo12:__boot_stack_core_zero_top
    mov sp, x0

    adrp x0, __bss_start
    add x0, x0, :lo12:__bss_start
    adrp x1, __bss_end
    add x1, x1, :lo12:__bss_end
clear_bss:
    cmp x0, x1
    b.hs bss_cleared
    str xzr, [x0], #8
    b clear_bss
bss_cleared:
    mov x0, x19
    bl kernel_main

park_boot_core:
    wfe
    b park_boot_core

    .section .rodata
    .balign 8
    .global secondary_entry_physical_address
secondary_entry_physical_address:
    .quad secondary_entry
