/* Raspberry Pi 4 Model B firmware boot layout.
 *
 * Firmware loads the raw kernel8.img at physical 0x0008_0000.
 * The boot stub and its two boot tables run at physical addresses. The rest
 * executes at physical address + KERNEL_OFFSET using TTBR1_EL1.
 *
 * KERNEL_OFFSET MUST match KERNEL_VIRTUAL_OFFSET in address_translation.rs.
 * The early Pi 4 mapper is currently restricted to the first 1 GiB of RAM.
 */

ENTRY(_start)

KERNEL_PHYSICAL_BASE     = 0x00080000;
KERNEL_OFFSET            = 0xFFFFFF8000000000;

PAGE_SIZE                = 4096;
CORE_COUNT               = 8;

BOOT_STACK_SIZE          = 16384;
BOOT_GUARD_SIZE          = PAGE_SIZE;
BOOT_STACK_STRIDE        = BOOT_STACK_SIZE + BOOT_GUARD_SIZE;

IDLE_STACK_SIZE          = 8192;
IDLE_GUARD_SIZE          = PAGE_SIZE;
IDLE_STACK_STRIDE        = IDLE_STACK_SIZE + IDLE_GUARD_SIZE;

INTERRUPT_STACK_SIZE     = 65536;
INTERRUPT_GUARD_SIZE     = PAGE_SIZE;
INTERRUPT_STACK_STRIDE   = INTERRUPT_STACK_SIZE + INTERRUPT_GUARD_SIZE;

RECLAMATION_STACK_SIZE   = 65536;
RECLAMATION_STACK_STRIDE = RECLAMATION_STACK_SIZE + INTERRUPT_GUARD_SIZE;

__interrupt_stack_stride = INTERRUPT_STACK_STRIDE;
__boot_stack_size        = BOOT_STACK_SIZE;
__boot_stack_stride      = BOOT_STACK_STRIDE;
__interrupt_stack_size   = INTERRUPT_STACK_SIZE;

SECTIONS
{
    . = KERNEL_PHYSICAL_BASE;

    .text.boot : {
        KEEP(*(.text.boot))
    }

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
        __boot_stack_core_zero_top = . + BOOT_STACK_STRIDE;
        . += CORE_COUNT * BOOT_STACK_STRIDE;
        __boot_stacks_top = .;
    }

    .idle_stacks (NOLOAD) : ALIGN(PAGE_SIZE) {
        __idle_stacks_bottom = .;
        . += CORE_COUNT * IDLE_STACK_STRIDE;
        __idle_stacks_top = .;
    }

    .interrupt_stacks (NOLOAD) : ALIGN(PAGE_SIZE) {
        __interrupt_stacks_bottom = .;
        . += CORE_COUNT * INTERRUPT_STACK_STRIDE;
        __interrupt_stacks_top = .;
    }

    .reclamation_stacks (NOLOAD) : ALIGN(PAGE_SIZE) {
        __reclamation_stacks_bottom = .;
        . += CORE_COUNT * RECLAMATION_STACK_STRIDE;
        __reclamation_stacks_top = .;
    }

    . = ALIGN(PAGE_SIZE);
    __kernel_end = .;

    /* Do not start using an out-of-range RAM region before the dynamic map exists. */
    ASSERT(__kernel_end - KERNEL_OFFSET <= 0x40000000,
           "Raspberry Pi 4 bootstrap exceeds the first 1 GiB of RAM")

    /DISCARD/ : { *(.comment) *(.eh_frame*) }
}
