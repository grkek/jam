/* Memory layout for QEMU `virt` (RAM begins at 0x4000_0000).
 *
 * The kernel is loaded at a physical address but runs in the upper half of the address space,
 * at physical address + KERNEL_OFFSET. Only the boot code and the boot translation tables sit
 * at their physical addresses, because they run before translation is on.
 *
 * KERNEL_OFFSET must match KERNEL_VIRTUAL_OFFSET in memory/address_translation.rs.
 */

ENTRY(_start)

KERNEL_PHYSICAL_BASE = 0x40200000;
KERNEL_OFFSET        = 0xFFFFFF8000000000;
PAGE_SIZE            = 4096;
CORE_COUNT           = 8;
BOOT_STACK_SIZE      = 16384;
INTERRUPT_STACK_SIZE = 16384;

__boot_stack_size      = BOOT_STACK_SIZE;
__interrupt_stack_size = INTERRUPT_STACK_SIZE;

SECTIONS
{
    . = KERNEL_PHYSICAL_BASE;

    /* Runs with translation off, so it must sit at its physical address. */
    .text.boot : {
        KEEP(*(.text.boot))
    }

    /* Two tables, one per half of the address space, built by the boot code. */
    .boot_tables (NOLOAD) : ALIGN(PAGE_SIZE) {
        __boot_tables_start = .;
        . += 2 * PAGE_SIZE;
        __boot_tables_end = .;
    }

    . = ALIGN(PAGE_SIZE);
    . = . + KERNEL_OFFSET;

    .text : AT(ADDR(.text) - KERNEL_OFFSET) {
        __text_start = .;
        *(.text .text.*)
        . = ALIGN(PAGE_SIZE);
        __text_end = .;
    }

    .rodata : ALIGN(PAGE_SIZE) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        . = ALIGN(PAGE_SIZE);
        __rodata_end = .;
    }

    .data : ALIGN(PAGE_SIZE) {
        *(.data .data.*)
    }

    .bss : ALIGN(PAGE_SIZE) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(8);
        __bss_end = .;
    }

    .boot_stacks (NOLOAD) : ALIGN(PAGE_SIZE) {
        __boot_stacks_bottom = .;
        __boot_stack_core_zero_top = . + BOOT_STACK_SIZE;
        . += CORE_COUNT * BOOT_STACK_SIZE;
        __boot_stacks_top = .;
    }

    .interrupt_stacks (NOLOAD) : ALIGN(PAGE_SIZE) {
        __interrupt_stacks_bottom = .;
        . += CORE_COUNT * INTERRUPT_STACK_SIZE;
        __interrupt_stacks_top = .;
    }

    . = ALIGN(PAGE_SIZE);
    __kernel_end = .;

    /DISCARD/ : { *(.comment) *(.eh_frame*) }
}
