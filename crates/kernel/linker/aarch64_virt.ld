/* Memory layout for QEMU `virt` (RAM begins at 0x4000_0000). */

ENTRY(_start)

KERNEL_LOAD_ADDRESS  = 0x40200000;
PAGE_SIZE            = 4096;
CORE_COUNT           = 8;
BOOT_STACK_SIZE      = 16384;
INTERRUPT_STACK_SIZE = 16384;

__boot_stack_size      = BOOT_STACK_SIZE;
__interrupt_stack_size = INTERRUPT_STACK_SIZE;

SECTIONS
{
    . = KERNEL_LOAD_ADDRESS;

    .text : {
        __text_start = .;
        KEEP(*(.text.boot))
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
